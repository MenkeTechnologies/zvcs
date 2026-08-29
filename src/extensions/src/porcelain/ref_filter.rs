//! The shared half of git's `ref-filter.c`, used by every verb that lists refs
//! through a `--format`.
//!
//! In git there is exactly one ref-listing engine. `for-each-ref`, `branch
//! --list` and `tag --list` all build a `struct ref_filter`, hand it to
//! `filter_refs()` / `filter_and_format_refs()`, and print whatever
//! `format_ref_array_item()` renders. The difference between the three verbs is
//! only *which* refs they ask for and *what format string they fall back to*:
//!
//! * `builtin/branch.c:469-470` — `if (!format->format) format->format =
//!   build_format(filter, maxwidth, remote_prefix);`
//! * `builtin/tag.c:62-70` — `%(refname:lstrip=2)`, or the `-n<num>` variant.
//!
//! This module is that engine's driver. It collects the refs a verb asked for,
//! applies the filters, and renders and sorts them through
//! [`super::for_each_ref`]'s evaluator — the port of `populate_value()`.
//! Callers get finished lines back and decide how to emit them (straight to
//! stdout, or through the column engine).
//!
//! Keeping one evaluator is not a tidiness preference. An atom rendered from a
//! second, thinner per-ref model produces a *wrong value at exit 0* — the worst
//! outcome for a command whose output is normally piped into a script.
//!
//! The run is deliberately two-phase, because git's is: `print_ref_list()`
//! filters the array, sizes the name column from what survived, *then* builds and
//! verifies the format string. A caller that needs that width supplies the format
//! as a closure over [`Candidate`] rather than a fixed string.

use anyhow::Result;
use std::collections::HashSet;
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::objs::Kind;

use super::for_each_ref::{
    self, filter_is_base, format_ref, is_packed, load, parse_atom, parse_format, passes_filters,
    peel_chain, short_name, sort_refs, Atom, AtomCtx, AtomError, ErrKind, Field, Filters, Item,
    NameMod, QuoteStyle, RefInfo, RenderCtx, SortKey,
};
use crate::refsort::Prereleases;

/// git's `FILTER_REFS_*` bits, as far as the ref-listing verbs use them.
///
/// Only the namespaces `apply_ref_filter()` tests against for `branch` and `tag`
/// are modelled; `for-each-ref` uses `FILTER_REFS_ALL` and never consults them.
pub(super) mod kind {
    pub(in crate::porcelain) const BRANCHES: u32 = 1 << 1;
    pub(in crate::porcelain) const TAGS: u32 = 1 << 2;
    pub(in crate::porcelain) const REMOTES: u32 = 1 << 3;
    pub(in crate::porcelain) const DETACHED_HEAD: u32 = 1 << 5;
    /// Everything else under `refs/`, which neither `branch` nor `tag` asks for.
    pub(in crate::porcelain) const OTHERS: u32 = 1 << 6;
}

/// `ref_kind_from_refname()` (ref-filter.c:2904-2931), restricted to the
/// namespaces `branch` and `tag` can ask for.
fn ref_kind_from_refname(refname: &[u8]) -> u32 {
    if refname.starts_with(b"refs/heads/") {
        kind::BRANCHES
    } else if refname.starts_with(b"refs/remotes/") {
        kind::REMOTES
    } else if refname.starts_with(b"refs/tags/") {
        kind::TAGS
    } else {
        kind::OTHERS
    }
}

/// `match_pattern()` (ref-filter.c:2670-2692) — the pattern rule `branch` and
/// `tag` use, which is *not* `for-each-ref`'s.
///
/// Two differences, both load-bearing: the well-known namespace prefix is
/// stripped from the ref name before matching (so `git branch --list feature/*`
/// is written against `feature/one`, not `refs/heads/feature/one`), and the
/// wildmatch runs without `WM_PATHNAME`, so `*` crosses `/`.
///
/// ```c
/// (void)(skip_prefix(refname, "refs/tags/", &refname) ||
///        skip_prefix(refname, "refs/heads/", &refname) ||
///        skip_prefix(refname, "refs/remotes/", &refname) ||
///        skip_prefix(refname, "refs/", &refname));
///
/// for (; *patterns; patterns++) {
///         if (!wildmatch(*patterns, refname, flags))
///                 return 1;
/// }
/// ```
fn match_pattern(pattern: &str, refname: &[u8], ignore_case: bool) -> bool {
    use gix::bstr::ByteSlice;
    let stripped = [
        b"refs/tags/".as_slice(),
        b"refs/heads/".as_slice(),
        b"refs/remotes/".as_slice(),
        b"refs/".as_slice(),
    ]
    .iter()
    .find_map(|p| refname.strip_prefix(*p))
    .unwrap_or(refname);
    let mut mode = gix::glob::wildmatch::Mode::empty();
    if ignore_case {
        mode |= gix::glob::wildmatch::Mode::IGNORE_CASE;
    }
    gix::glob::wildmatch(pattern.as_bytes().as_bstr(), stripped.as_bstr(), mode)
}

/// A ref that survived filtering, before any object body was read.
///
/// This is what a caller sees when it sizes a column: git's `calc_maxwidth()`
/// runs over exactly this set, in this (pre-sort) order.
pub(super) struct Candidate {
    pub(super) refname: Vec<u8>,
    /// `Some` only for the detached-HEAD pseudo entry, holding
    /// `get_head_description()`.
    pub(super) head_desc: Option<Vec<u8>>,
    /// git's `ref_kind_from_refname()` bit, so a caller can tell a remote-tracking
    /// ref apart without re-deriving it.
    pub(super) kind: u32,
    id: ObjectId,
    symref: Vec<u8>,
    packed: bool,
    /// The tag-peel chain, computed only when a filter or a `*`-atom needed it.
    chain: Vec<ObjectId>,
}

/// Where the format string comes from.
pub(super) enum Format<'a> {
    /// An explicit `--format`, which suppresses the verb's built-in one
    /// (`if (!format->format)`).
    Fixed(Vec<u8>),
    /// The verb's own `build_format()`, called with the filtered ref set so it
    /// can size the name column.
    Built(&'a dyn Fn(&[Candidate]) -> Vec<u8>),
}

/// One `git branch --list` / `git tag --list` run, in the shape
/// `struct ref_filter` + `struct ref_format` + `struct ref_sorting` describe it.
pub(super) struct ListSpec<'a> {
    pub(super) repo: &'a gix::Repository,
    pub(super) format: Format<'a>,
    /// `--sort` specs in command-line order (config keys first, as git seeds
    /// them); the driver reverses them, since a later key takes precedence.
    pub(super) sort_specs: Vec<String>,
    /// `filter->kind`.
    pub(super) kinds: u32,
    /// `filter->name_patterns`, matched by `match_pattern()` — the
    /// prefix-stripping, non-`WM_PATHNAME` variant `branch` and `tag` use
    /// (ref-filter.c:2670-2692), never `for-each-ref`'s `match_as_path` form.
    pub(super) patterns: Vec<String>,
    /// `filter->ignore_case`, which is both a match flag and a sort flag.
    pub(super) ignore_case: bool,
    pub(super) points_at: Vec<ObjectId>,
    pub(super) filters: Filters,
    /// `format->array_opts.omit_empty`.
    pub(super) omit_empty: bool,
    /// `format->use_color` — already resolved against the tty by the caller.
    pub(super) color_on: bool,
    /// The `get_head_description()` pseudo entry `git branch --list` adds when
    /// HEAD is detached (`filter.kind |= FILTER_REFS_DETACHED_HEAD`,
    /// builtin/branch.c:869-870), or `None` when the verb does not ask for one.
    pub(super) head_desc: Option<Vec<u8>>,
    /// Whether `filter_is_base()` runs.
    ///
    /// It is *not* universal. `filter_and_format_refs()` calls it
    /// (ref-filter.c:3437-3441), so `for-each-ref` and `tag` mark a base; but
    /// `print_ref_list()` open-codes its own tail —
    ///
    /// ```c
    /// filter_ahead_behind(the_repository, &array);
    /// ref_array_sort(sorting, &array);
    /// ```
    ///
    /// (builtin/branch.c:476-477) — with no `filter_is_base()` call at all, so
    /// `git branch --format='%(is-base:<x>)'` renders empty for *every* branch,
    /// including the one `git for-each-ref` picks out of the same refs.
    pub(super) run_is_base: bool,
    /// `REF_SORTING_DETACHED_HEAD_FIRST`. It is a flag *on the sorting nodes*
    /// (`ref_sorting_set_sort_flags_all`, builtin/branch.c:881-882), so it has
    /// no effect when there are none: `ref_array_sort()` runs only
    /// `if (sorting)`, and with an empty `--sort` list the detached pseudo entry
    /// keeps the position `do_filter_refs()` gave it, which is last.
    pub(super) detached_head_first: bool,
    /// `filter->verbose` (`git branch -v`). It joins the reachability filters in
    /// `apply_ref_filter()`'s one object lookup:
    ///
    /// ```c
    /// if (filter->reachable_from || filter->unreachable_from ||
    ///     filter->with_commit || filter->no_commit || filter->verbose) {
    ///         commit = lookup_commit_reference_gently(the_repository, ref->oid, 1);
    ///         if (!commit)
    ///                 return NULL;
    /// ```
    ///
    /// (ref-filter.c:2987-2991.) *Gently*, and the ref is dropped when the object
    /// is not there — which is why `git branch -v` in a repository with a branch
    /// pointing at a missing object lists the healthy branches and says nothing
    /// about the broken one, while `git branch --list` still names it: without
    /// `-v` nothing opens the object at all.
    pub(super) verbose: bool,
}

/// What a listing produced: rendered lines, or the exit code a format/sort parse
/// error implies (already reported on stderr).
pub(super) enum Listing {
    Lines(Vec<Vec<u8>>),
    Exit(ExitCode),
}

/// git's `print_ref_list()` / `filter_and_format_refs()`: filter, size, verify,
/// sort, render.
pub(super) fn filter_and_format(spec: &ListSpec<'_>) -> Result<Listing> {
    let repo = spec.repo;

    // Neither `builtin/branch.c` nor `builtin/tag.c` registers `OPT_QUOTING`, so
    // `--shell`/`--perl`/`--python`/`--tcl` never reach these verbs (they are
    // `error: unknown option`) and every format renders under `REF_FORMAT_INIT`.
    let quote_style = QuoteStyle::None;

    // git parses the sort keys in `cmd_branch()` / `cmd_tag()`, before
    // `print_ref_list()` ever verifies the format, so a bad key is reported first.
    let mut sorts: Vec<SortKey> = Vec::new();
    for raw in &spec.sort_specs {
        let (rest, descending) = match raw.strip_prefix('-') {
            Some(r) => (r, true),
            None => (raw.as_str(), false),
        };
        let (rest, versioned) = match rest
            .strip_prefix("version:")
            .or_else(|| rest.strip_prefix("v:"))
        {
            Some(r) => (r, true),
            None => (rest, false),
        };
        // A sort key parses through a fresh `REF_FORMAT_INIT`, never the
        // format's own state.
        let sort_ctx = AtomCtx {
            repo: Some(repo),
            color_on: spec.color_on,
            quote_style: QuoteStyle::None,
        };
        match parse_atom(rest, &sort_ctx) {
            Ok(atom) => sorts.push(SortKey {
                atom,
                descending,
                versioned,
            }),
            Err(e) => return Ok(Listing::Exit(report(e)?)),
        }
    }
    // Later `--sort` options take precedence, so the last given key sorts first.
    sorts.reverse();

    // Phase 1: `filter_refs()`. Nothing here reads an object body.
    let candidates = filter_refs(spec, &sorts)?;

    // Phase 2: the format, which `build_format()` may size from what survived.
    let format = match &spec.format {
        Format::Fixed(f) => f.clone(),
        Format::Built(build) => build(&candidates),
    };
    let fmt_ctx = AtomCtx {
        repo: Some(repo),
        color_on: spec.color_on,
        quote_style,
    };
    let (items, color_reset_at_eol) = match parse_format(&format, &fmt_ctx) {
        Ok(v) => v,
        Err(e) => return Ok(Listing::Exit(report(e)?)),
    };

    // Phase 3: load what the atoms actually ask for, and render.
    let mut refs = populate(repo, candidates, &items, &sorts)?;
    let ctx = RenderCtx {
        repo,
        worktrees: std::cell::OnceCell::new(),
    };

    // `filter_ahead_behind()` (ref-filter.c:3187), which both `print_ref_list()`
    // (builtin/branch.c:476) and `filter_and_format_refs()` (ref-filter.c:3439)
    // run between filtering and sorting. It resolves every array item's refname
    // through `lookup_commit_reference_by_name()` — quiet *0* — so a ref that
    // does not peel to a commit prints `error: object %s is a %s, not a %s`
    // right here: before the first output line, in pre-sort order, and for every
    // ref in the array rather than only the ones the atom is rendered for.
    if !refs.is_empty() && atoms(&items, &sorts).any(|a| matches!(a.field, Field::AheadBehind(_))) {
        for info in &refs {
            let found = crate::objname::lookup_commit_reference(repo, info.obj.id);
            if let Some(note) = found.type_error() {
                eprintln!("error: {note}");
            }
        }
    }

    // `filter_is_base()` runs on the filtered array before the sort, exactly as
    // `filter_and_format_refs()` orders it (ref-filter.c:3437-3441) — for the
    // verbs that call it at all.
    if spec.run_is_base {
        let is_base_atoms: Vec<(String, ObjectId)> = atoms(&items, &sorts)
            .filter_map(|a| match &a.field {
                Field::IsBase(name, tip) => Some((name.clone(), *tip)),
                _ => None,
            })
            .collect();
        filter_is_base(repo, &mut refs, &is_base_atoms);
    }

    // `ref_array_sort()` (ref-filter.c:3556-3560) runs `QSORT_S` only
    // `if (sorting)`. An empty `--sort` list is therefore not "sort by refname" —
    // it is *no sort at all*, and the array keeps `do_filter_refs()`'s iteration
    // order. Falling back to a refname comparison here would be invisible for
    // `refs/` (which iterates in name order already) and wrong for the detached
    // HEAD entry, which is appended after that walk.
    let mut refs = if sorts.is_empty() {
        refs
    } else {
        let prereleases = Prereleases::new(repo);
        sort_refs(&ctx, refs, &sorts, spec.ignore_case, &prereleases)?
    };

    // `REF_SORTING_DETACHED_HEAD_FIRST` short-circuits `cmp_ref_sorting()` on the
    // *first* key whenever either side is the detached pseudo entry, and is
    // deliberately exempt from `REF_SORTING_REVERSE` (ref-filter.c:3485-3488,
    // 3524-3525). With a single such entry that reduces to "it sorts first",
    // whatever the keys say — as long as there is a key at all.
    if spec.detached_head_first {
        if let Some(at) = refs.iter().position(|r| r.head_desc.is_some()) {
            let head = refs.remove(at);
            refs.insert(0, head);
        }
    }

    let mut lines: Vec<Vec<u8>> = Vec::new();
    for info in &refs {
        let line = match format_ref(&ctx, &items, info, quote_style, color_reset_at_eol)? {
            Ok(line) => line,
            // Every stack error git raises while formatting reaches `die()`.
            Err(msg) => return Ok(Listing::Exit(for_each_ref::fatal(&msg))),
        };
        if spec.omit_empty && line.is_empty() {
            continue;
        }
        lines.push(line);
    }
    Ok(Listing::Lines(lines))
}

/// Every atom the run touches — the format's and the sort keys' alike, which is
/// how git decides what each ref has to be loaded with (`used_atom`).
fn atoms<'a>(items: &'a [Item], sorts: &'a [SortKey]) -> impl Iterator<Item = &'a Atom> {
    items
        .iter()
        .filter_map(|it| match it {
            Item::Atom(a) => Some(a),
            Item::Lit(_)
            | Item::AlignStart(_)
            | Item::IfStart(_)
            | Item::Then
            | Item::Else
            | Item::End => None,
        })
        .chain(sorts.iter().map(|s| &s.atom))
}

/// `filter_refs()`: walk the ref store and keep the refs this verb asked for.
fn filter_refs(spec: &ListSpec<'_>, sorts: &[SortKey]) -> Result<Vec<Candidate>> {
    let repo = spec.repo;
    let filters_active = spec.filters.active();
    // A `*`-prefixed sort key peels too, so the chain is worth keeping from here.
    let sort_derefs = sorts.iter().any(|s| s.atom.deref);

    let mut names: Vec<Vec<u8>> = Vec::new();
    for r in repo.references()?.all()? {
        let r = r.map_err(|e| anyhow::anyhow!("{e}"))?;
        names.push(r.name().as_bstr().to_vec());
    }
    // `do_filter_refs()` appends the HEAD pseudo entry *after* the `refs/` walk
    // (ref-filter.c:3339-3342).
    let head_at = names.len();
    if spec.head_desc.is_some() {
        names.push(b"HEAD".to_vec());
    }

    let mut out: Vec<Candidate> = Vec::new();
    for (n, refname) in names.into_iter().enumerate() {
        let detached = n == head_at && spec.head_desc.is_some();
        let this_kind = if detached {
            kind::DETACHED_HEAD
        } else {
            ref_kind_from_refname(&refname)
        };
        if this_kind & spec.kinds == 0 {
            continue;
        }
        // `filter_pattern_match()` runs on the *full* ref name; `match_pattern()`
        // strips the well-known prefixes itself before wildmatching. The detached
        // entry's name is `HEAD`, which no `refs/` prefix strips.
        if !spec.patterns.is_empty()
            && !spec
                .patterns
                .iter()
                .any(|p| match_pattern(p, &refname, spec.ignore_case))
        {
            continue;
        }

        let (symref, id, packed) = if detached {
            let Some(id) = repo.head()?.id().map(|i| i.detach()) else {
                continue;
            };
            (Vec::new(), id, false)
        } else {
            let Ok(name_str) = std::str::from_utf8(&refname) else {
                continue;
            };
            let mut reference = repo.find_reference(name_str)?;
            let symref = reference
                .target()
                .try_name()
                .map(|nm| nm.as_bstr().to_vec())
                .unwrap_or_default();
            // `do_for_each_ref()` drops a ref that does not resolve, so a dangling
            // symbolic ref is simply absent rather than an error.
            let Ok(id) = reference.follow_to_object().map(|i| i.detach()) else {
                continue;
            };
            (symref, id, is_packed(repo, name_str))
        };

        // `apply_ref_filter()`'s gentle commit lookup (ref-filter.c:2987-2991): the reachability
        // filters and `-v` all need the commit, and a ref whose object is missing is dropped here
        // rather than reported. Nothing else in the walk opens an object, so a listing that asks
        // for none still names a branch pointing at a missing object — which is what stock does.
        if (filters_active || spec.verbose) && repo.find_header(id).is_err() {
            continue;
        }

        let chain = if !spec.points_at.is_empty() || filters_active || sort_derefs {
            peel_chain(repo, id)?
        } else {
            Vec::new()
        };
        // `match_points_at()` accepts the ref's own id or any object it peels to.
        if !spec.points_at.is_empty()
            && !spec.points_at.iter().any(|t| *t == id || chain.contains(t))
        {
            continue;
        }
        if filters_active && !passes_filters(repo, &spec.filters, *chain.last().unwrap_or(&id))? {
            continue;
        }

        out.push(Candidate {
            refname,
            head_desc: if detached { spec.head_desc.clone() } else { None },
            kind: this_kind,
            id,
            symref,
            packed,
            chain,
        });
    }
    Ok(out)
}

/// Read each surviving ref's object as far as the run's atoms require, producing
/// the model `populate_value()` renders from.
fn populate(
    repo: &gix::Repository,
    candidates: Vec<Candidate>,
    items: &[Item],
    sorts: &[SortKey],
) -> Result<Vec<RefInfo>> {
    let needs_data = atoms(items, sorts).any(|a| {
        matches!(
            a.field,
            Field::Person(..)
                | Field::Contents(_)
                | Field::Tree(_)
                | Field::Parent(_)
                | Field::NumParent
                | Field::TargetName
                | Field::TargetType
                | Field::TagName
                | Field::Raw(_)
                | Field::Signature(_)
        )
    });
    let needs_peel = atoms(items, sorts).any(|a| a.deref);
    let needs_short = atoms(items, sorts).any(|a| matches!(a.field, Field::RefName(NameMod::Short)));
    let needs_symref_short =
        atoms(items, sorts).any(|a| matches!(a.field, Field::SymRef(NameMod::Short)));

    let head_name = repo.head_name()?.map(|n| n.as_bstr().to_vec());

    // The `:short` disambiguation rules test candidate names against every ref in
    // the repository, including the ones this verb's `kind` mask dropped.
    let all_names: HashSet<Vec<u8>> = if needs_short || needs_symref_short {
        let mut set = HashSet::new();
        for r in repo.references()?.all()? {
            let r = r.map_err(|e| anyhow::anyhow!("{e}"))?;
            set.insert(r.name().as_bstr().to_vec());
        }
        set
    } else {
        HashSet::new()
    };

    // Which atoms actually reach into the object. git opens none until one does — "We do not open
    // the object yet; sort may only need refname to do its job" (ref-filter.c:3002-3006) — so a
    // ref whose object is missing renders fine for `%(refname)`, and only a format that asks about
    // the object is `missing object %s for %s`.
    let needs_object = needs_data
        || needs_peel
        || atoms(items, sorts).any(|a| {
            matches!(
                a.field,
                Field::ObjectType | Field::ObjectSize
            )
        });

    let mut refs = Vec::with_capacity(candidates.len());
    for c in candidates {
        let obj = match load(repo, c.id, needs_data) {
            Ok(obj) => obj,
            // Nothing in this run will look at it, so the ref is listed by name — the state a
            // branch pointing at a missing object is in.
            Err(_) if !needs_object => super::for_each_ref::ObjInfo {
                id: c.id,
                kind: Kind::Commit,
                size: 0,
                data: None,
            },
            Err(err) => return Err(err),
        };
        let chain = if needs_peel && c.chain.is_empty() && obj.kind == Kind::Tag {
            peel_chain(repo, c.id)?
        } else {
            c.chain
        };
        let peeled = match (needs_peel, obj.kind, chain.last()) {
            (true, Kind::Tag, Some(&last)) => Some(load(repo, last, needs_data)?),
            _ => None,
        };
        let short = if needs_short {
            short_name(repo, &c.refname, &all_names)
        } else {
            Vec::new()
        };
        let symref_short = if needs_symref_short && !c.symref.is_empty() {
            short_name(repo, &c.symref, &all_names)
        } else {
            Vec::new()
        };

        refs.push(RefInfo {
            // The detached pseudo entry *is* HEAD, and `head_atom_parser()`
            // resolves `HEAD` to the literal name `HEAD` when it is not symbolic,
            // so `%(HEAD)` marks it.
            is_head: c.head_desc.is_some() || head_name.as_deref() == Some(c.refname.as_slice()),
            head_desc: c.head_desc,
            refname: c.refname,
            short,
            symref: c.symref,
            symref_short,
            obj,
            peeled,
            packed: c.packed,
            is_base: Vec::new(),
        });
    }
    Ok(refs)
}

/// `pretty_print_ref()` (ref-filter.c:3653-3671) — render one ref that was never
/// filtered or sorted, which is how `git verify-tag` and `git tag -v` print a
/// `--format`.
///
/// ```c
/// ref_item = new_ref_array_item(name, oid, peeled_oid);
/// ref_item->kind = ref_kind_from_refname(name);
/// if (format_ref_array_item(ref_item, format, &output, &err))
///         die("%s", err.buf);
/// ```
///
/// `name` is the operand as the user typed it, not a full ref name — which is
/// why `git verify-tag --format='%(refname)' signed-tag` prints `signed-tag` and
/// `%(refname:lstrip=2)` prints nothing. `new_ref_array_item()` leaves the flag
/// word zero, so `%(flag)` is empty and `%(symref)` is too, and it is handed a
/// NULL `peeled_oid`, so a `*`-atom peels lazily inside the formatter.
pub(super) fn pretty_print_ref(
    repo: &gix::Repository,
    name: &[u8],
    id: ObjectId,
    items: &[Item],
) -> Result<std::result::Result<Vec<u8>, ExitCode>> {
    let sorts: [SortKey; 0] = [];
    let needs_data = atoms(items, &sorts).any(|a| {
        matches!(
            a.field,
            Field::Person(..)
                | Field::Contents(_)
                | Field::Tree(_)
                | Field::Parent(_)
                | Field::NumParent
                | Field::TargetName
                | Field::TargetType
                | Field::TagName
                | Field::Raw(_)
                | Field::Signature(_)
        )
    });
    let needs_peel = atoms(items, &sorts).any(|a| a.deref);
    let needs_short = atoms(items, &sorts).any(|a| matches!(a.field, Field::RefName(NameMod::Short)));

    let obj = load(repo, id, needs_data)?;
    let peeled = match (needs_peel, obj.kind) {
        (true, Kind::Tag) => peel_chain(repo, id)?
            .last()
            .map(|&last| load(repo, last, needs_data))
            .transpose()?,
        _ => None,
    };
    let short = if needs_short {
        let mut all = HashSet::new();
        for r in repo.references()?.all()? {
            let r = r.map_err(|e| anyhow::anyhow!("{e}"))?;
            all.insert(r.name().as_bstr().to_vec());
        }
        short_name(repo, name, &all)
    } else {
        Vec::new()
    };
    let head_name = repo.head_name()?.map(|n| n.as_bstr().to_vec());

    let info = RefInfo {
        is_head: head_name.as_deref() == Some(name),
        head_desc: None,
        refname: name.to_vec(),
        short,
        symref: Vec::new(),
        symref_short: Vec::new(),
        obj,
        peeled,
        packed: false,
        is_base: Vec::new(),
    };
    let ctx = RenderCtx {
        repo,
        worktrees: std::cell::OnceCell::new(),
    };
    Ok(
        match format_ref(&ctx, items, &info, QuoteStyle::None, false)? {
            Ok(line) => Ok(line),
            Err(msg) => Err(for_each_ref::fatal(&msg)),
        },
    )
}

/// `verify_ref_format()` for the verbs that print one ref rather than a listing.
/// The caller supplies the usage block its own `usage_with_options()` would show.
pub(super) fn parse_one_format(
    repo: &gix::Repository,
    fmt: &str,
    usage: &str,
) -> std::result::Result<Vec<Item>, ExitCode> {
    let ctx = AtomCtx {
        repo: Some(repo),
        color_on: false,
        quote_style: QuoteStyle::None,
    };
    match parse_format(fmt.as_bytes(), &ctx) {
        Ok((items, _)) => Ok(items),
        // A malformed `%(` reaches `usage_with_options(<this verb's usage>)`,
        // while an unknown field name has already `die()`d inside
        // `parse_ref_filter_atom()` — the split [`report`] draws for a listing,
        // with a different block on the usage side.
        Err(e) if matches!(e.kind, ErrKind::Usage) => {
            eprintln!("error: {}", e.msg);
            eprint!("{usage}");
            Err(ExitCode::from(129))
        }
        Err(e) => Err(match e.kind {
            ErrKind::Fatal => for_each_ref::fatal(&e.msg),
            _ => {
                eprintln!("zvcs: {}", e.msg);
                ExitCode::from(1)
            }
        }),
    }
}

/// Report a format/sort parse failure the way `branch` and `tag` report it.
///
/// The malformed-`%(` case is the one that differs from `for-each-ref`. All three
/// verbs get the same `error: malformed format string %s` out of
/// `verify_ref_format()`, but they do different things with its return value:
///
/// ```c
/// /* builtin/for-each-ref.c */  if (verify_ref_format(&format)) usage_with_options(...);
/// /* builtin/branch.c:473 */    if (verify_ref_format(format))  die(_("unable to parse format string"));
/// /* builtin/tag.c:72 */        if (verify_ref_format(&format)) die(_("unable to parse format string"));
/// ```
///
/// So `for-each-ref` exits 129 with the whole usage block, while `branch` and
/// `tag` exit 128 with one more line and no block.
pub(super) fn report(e: AtomError) -> Result<ExitCode> {
    if matches!(e.kind, ErrKind::Usage) {
        eprintln!("error: {}", e.msg);
        return Ok(for_each_ref::fatal("unable to parse format string"));
    }
    for_each_ref::report_atom_error(e)
}

/// `ref_sorting_options()` (ref-filter.c:3707-3724), which every verb runs while
/// parsing options — before it knows what mode it is in, so an invalid `--sort`
/// is fatal for `git tag -d` and `git tag -v` just as it is for `git tag -l`.
pub(super) fn check_sort(
    repo: &gix::Repository,
    specs: &[String],
) -> std::result::Result<(), AtomError> {
    for raw in specs {
        let rest = raw.strip_prefix('-').unwrap_or(raw);
        let rest = rest
            .strip_prefix("version:")
            .or_else(|| rest.strip_prefix("v:"))
            .unwrap_or(rest);
        let ctx = AtomCtx {
            repo: Some(repo),
            color_on: false,
            quote_style: QuoteStyle::None,
        };
        parse_atom(rest, &ctx)?;
    }
    Ok(())
}
