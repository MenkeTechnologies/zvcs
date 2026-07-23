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
//! `--no-refs`, `--no-exclude`, `--name-only`, `--all`, `--peel-tag`,
//! `--annotate-stdin` (and the deprecated `--stdin`), `--undefined`/
//! `--no-undefined`, `--always`, `-h`, plus the "Skipping." / `undefined` /
//! `fatal: cannot describe` failure paths and their exit codes (0 / 0 / 128) and
//! the usage errors (129).
//!
//! `--all` prints one line per commit in the order of git's *parsed-object hash
//! table* (`builtin/name-rev.c`'s `get_indexed_object` loop). That table is not
//! an implementation detail we can ignore: it is the output order. It is however
//! fully determined by the objects git parses and the order it parses them in,
//! so `Pool` below reproduces `object.c` exactly — `hash_obj` (the first four
//! bytes of the object id read as a native `unsigned int`, masked), the linear
//! probe, the move-to-home-slot swap in `lookup_object`, the `size - 1 <= nr * 2`
//! growth rule and its rehash — and the naming pass drives it through the same
//! `parse_object` / `repo_parse_commit` call sequence git uses. The pool doubles
//! as the commit cache, so each object is still read at most once.
//!
//! The commit-graph changes that table: `repo_parse_commit` served from the
//! graph never allocates the commit's tree object, which moves every later slot,
//! so `Pool::parse_commit` skips the tree for exactly the commits the graph
//! covers. Known deviation: git also prefers commit-graph generation numbers
//! over commit dates when deciding the traversal cutoff, whereas the walk here
//! is date-based only, so a repository with a commit-graph *and* badly skewed
//! commit dates can prune a different set.
//!
//! One further tie-break is git's and not reproducible in principle: `name_tips`
//! orders the tip table with `QSORT`, which is unstable, so tips with the same
//! `from_tag` and the same tagger date are visited in an order libc chooses. The
//! stable sort used here is one of the orders git may pick.

use anyhow::Result;
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
    /// Pool slot of the commit the ref peels to, if any; tips without one never
    /// name anything.
    commit: Option<usize>,
    taggerdate: i64,
    from_tag: bool,
    /// Whether an annotated tag was dereferenced to reach `commit` (adds `^0`).
    deref: bool,
}

/// git's `struct object`, plus the decoded facts of the kinds we parse.
struct Obj {
    oid: ObjectId,
    kind: Kind,
    parsed: bool,
    /// Committer date for a commit, tagger date for a tag, `0` otherwise.
    date: i64,
    /// Pool slots of the parents, in order (commits only).
    parents: Vec<usize>,
    /// Pool slot of the tagged object (tags only).
    tagged: Option<usize>,
}

/// git's `struct parsed_object_pool`: the open-addressed object hash table from
/// `object.c`, whose slot order *is* the `--all` output order.
struct Pool {
    /// Every object created, in creation order. Slots hold indices into this.
    objs: Vec<Obj>,
    /// The hash table itself; length is always a power of two, or zero.
    slots: Vec<Option<usize>>,
    /// git's `nr_objs`, kept signed to mirror the growth comparison exactly.
    nr: i64,
    /// Side index for lookups that git performs on a pointer it already holds,
    /// so they cannot perturb `slots` the way `lookup_object` would.
    index: HashMap<ObjectId, usize>,
    /// The commit-graph, when one exists and `core.commitGraph` allows it.
    /// `repo_parse_commit` served from it never allocates the commit's tree.
    graph: Option<gix::commitgraph::Graph>,
}

/// git's `hash_obj`: the first `sizeof(unsigned int)` bytes of the object id,
/// read with the machine's byte order, masked to the table size.
fn hash_obj(oid: &ObjectId, size: usize) -> usize {
    let b = oid.as_bytes();
    u32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as usize & (size - 1)
}

impl Pool {
    fn new(repo: &gix::Repository) -> Self {
        Pool {
            objs: Vec::new(),
            slots: Vec::new(),
            nr: 0,
            index: HashMap::new(),
            graph: repo.commit_graph_if_enabled().ok().flatten(),
        }
    }

    /// git's `lookup_object`, including the swap that moves a found object back
    /// to its home slot — that swap reorders the table, so it must be replayed.
    fn lookup(&mut self, oid: &ObjectId) -> Option<usize> {
        let size = self.slots.len();
        if size == 0 {
            return None;
        }
        let first = hash_obj(oid, size);
        let mut i = first;
        let mut found = None;
        while let Some(ix) = self.slots[i] {
            if &self.objs[ix].oid == oid {
                found = Some(ix);
                break;
            }
            i += 1;
            if i == size {
                i = 0;
            }
        }
        if found.is_some() && i != first {
            self.slots.swap(i, first);
        }
        found
    }

    /// Non-mutating lookup, for the reporting paths where git already holds the
    /// object pointer and performs no table access at all.
    fn find(&self, oid: &ObjectId) -> Option<usize> {
        self.index.get(oid).copied()
    }

    /// git's `grow_object_hash`: reinsert every live entry, walking the old
    /// table in slot order, which is what makes the new layout deterministic.
    fn grow(&mut self) {
        let new_size = if self.slots.len() < 32 {
            32
        } else {
            self.slots.len() * 2
        };
        let mut new_slots: Vec<Option<usize>> = vec![None; new_size];
        for slot in &self.slots {
            let Some(ix) = *slot else { continue };
            let mut j = hash_obj(&self.objs[ix].oid, new_size);
            while new_slots[j].is_some() {
                j += 1;
                if j >= new_size {
                    j = 0;
                }
            }
            new_slots[j] = Some(ix);
        }
        self.slots = new_slots;
    }

    /// git's `create_object`, growth rule included.
    fn create(&mut self, oid: ObjectId, kind: Kind) -> usize {
        if self.slots.len() as i64 - 1 <= self.nr * 2 {
            self.grow();
        }
        let ix = self.objs.len();
        self.objs.push(Obj {
            oid,
            kind,
            parsed: false,
            date: 0,
            parents: Vec::new(),
            tagged: None,
        });
        let size = self.slots.len();
        let mut j = hash_obj(&oid, size);
        while self.slots[j].is_some() {
            j += 1;
            if j >= size {
                j = 0;
            }
        }
        self.slots[j] = Some(ix);
        self.nr += 1;
        self.index.insert(oid, ix);
        ix
    }

    /// git's `lookup_commit` / `lookup_tree` / `lookup_blob` / `lookup_tag`.
    fn lookup_typed(&mut self, oid: ObjectId, kind: Kind) -> usize {
        match self.lookup(&oid) {
            Some(ix) => ix,
            None => self.create(oid, kind),
        }
    }

    /// git's `parse_object`, whose side effect — creating the tree and parent
    /// objects a commit names, or the object a tag names — is what fills the
    /// table.
    fn parse_object(&mut self, repo: &gix::Repository, oid: ObjectId) -> Option<usize> {
        let existing = self.lookup(&oid);
        if let Some(ix) = existing {
            if self.objs[ix].parsed {
                return Some(ix);
            }
        }

        // git streams blobs rather than reading them, and re-looks-up the result.
        let blob_like = existing.is_none_or(|ix| self.objs[ix].kind == Kind::Blob);
        if blob_like && matches!(repo.find_header(oid), Ok(h) if h.kind() == Kind::Blob) {
            let ix = self.lookup_typed(oid, Kind::Blob);
            self.objs[ix].parsed = true;
            return self.lookup(&oid);
        }

        let object = repo.find_object(oid).ok()?;
        let ix = self.lookup_typed(oid, object.kind);
        match object.kind {
            // `parse_object` never consults the commit-graph, so the tree is
            // always created here.
            Kind::Commit => self.parse_commit_buffer(ix, &object.data, true),
            Kind::Tag => self.parse_tag_buffer(ix, &object.data),
            _ => self.objs[ix].parsed = true,
        }
        Some(ix)
    }

    /// git's `parse_commit_buffer`: the tree object is created first, then each
    /// parent in order. A commit that will not decode stays unparsed, exactly as
    /// git leaves it after `error("bogus commit object")`.
    ///
    /// `create_tree` is false for the commit-graph path, where git's
    /// `fill_commit_in_graph` sets the tree to NULL instead of looking it up.
    fn parse_commit_buffer(&mut self, ix: usize, data: &[u8], create_tree: bool) {
        if self.objs[ix].parsed {
            return;
        }
        let hash = self.objs[ix].oid.kind();
        let Ok(commit) = gix::objs::CommitRef::from_bytes(data, hash) else {
            return;
        };
        let tree = commit.tree();
        let parents: Vec<ObjectId> = commit.parents().collect();
        let date = commit.committer().ok().map_or(0, |s| s.seconds());

        if create_tree {
            self.lookup_typed(tree, Kind::Tree);
        }
        let parents: Vec<usize> = parents
            .into_iter()
            .map(|p| self.lookup_typed(p, Kind::Commit))
            .collect();

        let obj = &mut self.objs[ix];
        obj.parents = parents;
        obj.date = date;
        obj.parsed = true;
    }

    /// git's `parse_tag_buffer`: creates the tagged object with the type the tag
    /// header claims, and records the tagger date (`0` when there is no tagger).
    fn parse_tag_buffer(&mut self, ix: usize, data: &[u8]) {
        if self.objs[ix].parsed {
            return;
        }
        let hash = self.objs[ix].oid.kind();
        let Ok(tag) = gix::objs::TagRef::from_bytes(data, hash) else {
            return;
        };
        let date = tag.tagger().ok().flatten().map_or(0, |t| t.seconds());
        let tagged = self.lookup_typed(tag.target(), tag.target_kind);

        let obj = &mut self.objs[ix];
        obj.tagged = Some(tagged);
        obj.date = date;
        obj.parsed = true;
    }

    /// git's `repo_parse_commit`: the walk holds the commit already, so no table
    /// lookup happens — only the parse, and the objects it creates.
    ///
    /// Unlike `parse_object`, this path is served from the commit-graph when one
    /// covers the commit. The parents and the date are the same either way; what
    /// differs is that no tree object is created, which changes the table.
    fn parse_commit(&mut self, repo: &gix::Repository, ix: usize) {
        if self.objs[ix].parsed {
            return;
        }
        let oid = self.objs[ix].oid;
        let in_graph = self.graph.as_ref().is_some_and(|g| g.lookup(oid).is_some());
        if let Ok(object) = repo.find_object(oid) {
            if object.kind == Kind::Commit {
                self.parse_commit_buffer(ix, &object.data, !in_graph);
            }
        }
    }

    /// git's `deref_tag`: follow the tag chain to the object it finally names.
    fn deref_tag(&mut self, repo: &gix::Repository, start: usize) -> Option<usize> {
        let mut current = Some(start);
        while let Some(ix) = current {
            if self.objs[ix].kind != Kind::Tag {
                break;
            }
            let tagged = self.objs[ix].tagged?;
            let oid = self.objs[tagged].oid;
            current = self.parse_object(repo, oid);
        }
        current
    }
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
    let mut peel_tag = false;
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
            "-h" => {
                // git's `parse_options` prints the usage on stdout and exits 129.
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
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
                let Some(v) = args.get(i) else {
                    // git's `parse_options`: the message alone, no usage block.
                    eprintln!("error: option `{}' requires a value", &a[2..]);
                    return Ok(ExitCode::from(129));
                };
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
            "--peel-tag" => peel_tag = true,
            "--no-peel-tag" => peel_tag = false,
            _ => {
                // git's `parse_options` rejects the token itself, then prints the
                // usage block, and exits 129.
                match a.strip_prefix("--") {
                    Some(rest) => eprintln!("error: unknown option `{rest}'"),
                    // Short options are parsed one at a time, so only the first
                    // character of the cluster is named.
                    None => {
                        let c = a.chars().nth(1).unwrap_or('-');
                        eprintln!("error: unknown switch `{c}'");
                    }
                }
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        }
        i += 1;
    }

    // git: `if (all + annotate_stdin + !!argc > 1)` -> error + usage, exit 129.
    if (all as u8) + (annotate_stdin as u8) + u8::from(!revs.is_empty()) > 1 {
        eprintln!("error: Specify either a list, or --all, not both!");
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let repo = gix::discover(".")?;
    let hexsz = repo.object_hash().len_in_hex();

    // git disables the cutoff entirely for --all and --annotate-stdin.
    let mut cutoff: i64 = if all || annotate_stdin { 0 } else { TIME_MAX };

    let mut pool = Pool::new(&repo);
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
        let Some(ix) = pool.parse_object(&repo, oid) else {
            eprintln!("Could not get object for {spec}. Skipping.");
            continue;
        };
        let peeled = pool.deref_tag(&repo, ix);
        let commit = peeled.filter(|&c| pool.objs[c].kind == Kind::Commit);
        if let Some(c) = commit {
            let date = pool.objs[c].date;
            if cutoff > date {
                cutoff = date;
            }
        }
        // `--peel-tag` reports the commit a tag names in place of the tag.
        if peel_tag {
            let Some(c) = commit else {
                eprintln!("Could not get commit for {spec}. Skipping.");
                continue;
            };
            targets.push((spec.clone(), pool.objs[c].oid, Kind::Commit));
            continue;
        }
        targets.push((spec.clone(), oid, pool.objs[ix].kind));
    }

    // Apply the clock-skew slop (git's `adjust_cutoff_timestamp_for_slop`).
    if cutoff != 0 {
        cutoff = cutoff.saturating_sub(CUTOFF_DATE_SLOP);
    }

    let tips = collect_tips(
        &repo,
        &mut pool,
        tags_only,
        name_only,
        &ref_filters,
        &exclude_filters,
    )?;

    // "Try to set better names first, so that worse ones spread less."
    let mut order: Vec<usize> = (0..tips.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&tips[a], &tips[b]);
        b.from_tag
            .cmp(&a.from_tag)
            .then(a.taggerdate.cmp(&b.taggerdate))
    });

    let mut names: HashMap<usize, RevName> = HashMap::new();
    for ix in order {
        let tip = &tips[ix];
        if let Some(commit) = tip.commit {
            walk_from_tip(&repo, &mut pool, &mut names, commit, tip, cutoff);
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

    // git's `get_rev_name`: a commit is named by the walk, anything else only by
    // an exact ref match.
    let rev_name = |oid: &ObjectId, kind: Kind| -> Option<String> {
        if kind == Kind::Commit {
            pool.find(oid).and_then(|ix| names.get(&ix)).map(render_name)
        } else {
            exact(oid)
        }
    };

    if annotate_stdin {
        annotate(&mut out, hexsz, name_only, &pool, &names, &exact)?;
        out.flush()?;
        return Ok(ExitCode::SUCCESS);
    }

    if all {
        // git walks its object hash table in slot order, so this is the order.
        for slot in &pool.slots {
            let Some(ix) = *slot else { continue };
            if pool.objs[ix].kind != Kind::Commit {
                continue;
            }
            let oid = pool.objs[ix].oid;
            if !name_only {
                write!(out, "{oid} ")?;
            }
            let name = names.get(&ix).map(render_name);
            if !emit_name(&mut out, &repo, oid, name, allow_undefined, always)? {
                return Ok(ExitCode::from(128));
            }
        }
        out.flush()?;
        return Ok(ExitCode::SUCCESS);
    }

    for (caller, oid, kind) in &targets {
        if !name_only {
            write!(out, "{caller} ")?;
        }
        let name = rev_name(oid, *kind);
        if !emit_name(&mut out, &repo, *oid, name, allow_undefined, always)? {
            return Ok(ExitCode::from(128));
        }
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// The tail of git's `show_name`: the name, or the `undefined` / abbreviated /
/// `fatal` fallbacks in git's order. Returns `false` where git would have died,
/// after emitting the message it dies with.
fn emit_name<W: Write>(
    out: &mut W,
    repo: &gix::Repository,
    oid: ObjectId,
    name: Option<String>,
    allow_undefined: bool,
    always: bool,
) -> Result<bool> {
    match name {
        Some(name) => writeln!(out, "{name}")?,
        None if allow_undefined => writeln!(out, "undefined")?,
        None if always => {
            let short = oid.attach(repo).shorten_or_id();
            writeln!(out, "{short}")?;
        }
        None => {
            // git dies with the partial line already written to stdout.
            out.flush()?;
            eprintln!("fatal: cannot describe '{oid}'");
            return Ok(false);
        }
    }
    Ok(true)
}

/// Build the tip table: one entry per ref that survives the `--tags`,
/// `--exclude` and `--refs` filters, mirroring git's `name_ref`.
fn collect_tips(
    repo: &gix::Repository,
    pool: &mut Pool,
    tags_only: bool,
    name_only: bool,
    ref_filters: &[String],
    exclude_filters: &[String],
) -> Result<Vec<Tip>> {
    // git's ref iterator yields refs in byte order, and with `--all` that order
    // reaches the output through the object table, so sort rather than trust the
    // backend. `shorten_unambiguous` also needs the full set of names up front.
    let platform = repo.references()?;
    let mut refs = Vec::new();
    for reference in platform.all()? {
        refs.push(reference.map_err(|e| anyhow::anyhow!("{e}"))?);
    }
    refs.sort_by(|a, b| a.name().as_bstr().cmp(b.name().as_bstr()));
    let all_names: HashSet<String> = refs
        .iter()
        .map(|r| r.name().as_bstr().to_string())
        .collect();

    let mut tips = Vec::new();
    for reference in &mut refs {
        let full = reference.name().as_bstr().to_string();
        let is_tag_ref = full.starts_with("refs/tags/");

        // Symbolic refs are followed; refs whose object is missing are still
        // recorded (git keeps them in the tip table with no commit).
        let Ok(id) = reference.follow_to_object() else {
            continue;
        };
        let oid = id.detach();

        // git parses the ref's object before any filter runs, and the objects
        // that parse creates are part of the `--all` table even when the ref
        // itself is filtered out.
        let peeled = pool.parse_object(repo, oid);

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

        // Peel the tag chain, remembering the innermost tagger date. A tag whose
        // target is unreadable stops the chain on the tag itself, as git's
        // `break` on a missing `t->tagged` does.
        let mut taggerdate = TIME_MAX;
        let mut deref = false;
        let mut peeled = peeled;
        while let Some(ix) = peeled {
            if pool.objs[ix].kind != Kind::Tag {
                break;
            }
            let Some(tagged) = pool.objs[ix].tagged else {
                break;
            };
            taggerdate = pool.objs[ix].date;
            deref = true;
            let target = pool.objs[tagged].oid;
            peeled = pool.parse_object(repo, target);
        }

        let mut commit = None;
        let mut from_tag = false;
        if let Some(ix) = peeled {
            if pool.objs[ix].kind == Kind::Commit {
                commit = Some(ix);
                from_tag = is_tag_ref;
                if taggerdate == TIME_MAX {
                    taggerdate = pool.objs[ix].date;
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
    pool: &mut Pool,
    names: &mut HashMap<usize, RevName>,
    start: usize,
    tip: &Tip,
    cutoff: i64,
) {
    pool.parse_commit(repo, start);
    if pool.objs[start].date < cutoff {
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
    let mut pending: Vec<usize> = Vec::new();
    while let Some(commit) = stack.pop() {
        let Some(name) = names.get(&commit) else {
            continue;
        };
        let (cur_tip, cur_gen, cur_dist) = (name.tip_name.clone(), name.generation, name.distance);
        let parents = pool.objs[commit].parents.clone();

        pending.clear();
        for (ix, parent) in parents.iter().enumerate() {
            let parent_number = ix as i32 + 1;
            let parent = *parent;
            pool.parse_commit(repo, parent);
            if pool.objs[parent].date < cutoff {
                continue;
            }
            let (generation, distance) = if parent_number > 1 {
                (0, cur_dist.saturating_add(MERGE_TRAVERSAL_WEIGHT))
            } else {
                (cur_gen.saturating_add(1), cur_dist.saturating_add(1))
            };
            if !is_better_name(
                names.get(&parent),
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
                parent,
                RevName {
                    tip_name: parent_tip,
                    taggerdate: tip.taggerdate,
                    generation,
                    distance,
                    from_tag: tip.from_tag,
                },
            );
            pending.push(parent);
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
    pool: &Pool,
    names: &HashMap<usize, RevName>,
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
            let named = pool.find(&oid).and_then(|ix| names.get(&ix)).map(render_name);
            let Some(name) = named.or_else(|| exact(&oid)) else {
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
