//! `--filter=<spec>`: git's `list-objects-filter-options.c` (the parser) and
//! `list-objects-filter.c` (the per-object decisions a traversal asks for).
//!
//! The parser is shared by every command that takes a filter spec —
//! `pack-objects` and `gc`/`repack` validate with it, `rev-list` walks with it —
//! so there is one grammar and one set of diagnostics.
//!
//! The runtime half mirrors git's `struct filter`: a traversal offers each
//! object to [`filter_object`] together with the situation it is in
//! (`LOFS_COMMIT`, `LOFS_TAG`, `LOFS_BEGIN_TREE`, `LOFS_END_TREE`, `LOFS_BLOB`)
//! and acts on the `LOFR_*` bits it gets back.

use std::collections::{HashMap, HashSet};

use gix::bstr::{BStr, ByteSlice};
use gix::glob::{pattern::Case, wildmatch::Mode as WildMode, Pattern};
use gix::hash::ObjectId;
use gix::object::Kind;

/// `struct list_objects_filter_options` once parsed: the `choice` together with
/// the one value that choice reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FilterOptions {
    /// `LOFC_AUTO`: resolved by the caller before any traversal.
    Auto,
    /// `LOFC_BLOB_NONE`.
    BlobNone,
    /// `LOFC_BLOB_LIMIT`: `blob_limit_value`.
    BlobLimit(u64),
    /// `LOFC_TREE_DEPTH`: `tree_exclude_depth`.
    TreeDepth(u64),
    /// `LOFC_SPARSE_OID`: `sparse_oid_name`, resolved when the filter is built.
    SparseOid(Vec<u8>),
    /// `LOFC_OBJECT_TYPE`: `object_type`.
    ObjectType(Kind),
    /// `LOFC_COMBINE`: `sub[0..sub_nr]`.
    Combine(Vec<FilterOptions>),
}

/// `gently_parse_list_objects_filter` (list-objects-filter-options.c:44):
/// match the spec against git's fixed set of filter forms, in git's declaration
/// order (which decides which diagnostic a near-miss like `blob:` or `object:`
/// gets). `Err(msg)` carries the exact text git puts after `fatal: `.
pub(crate) fn gently_parse_list_objects_filter(
    arg: &[u8],
    allow_auto_filter: bool,
) -> Result<FilterOptions, String> {
    if arg == b"auto" {
        if !allow_auto_filter {
            return Err("'auto' filter not supported by this command".to_string());
        }
        return Ok(FilterOptions::Auto);
    }
    if arg == b"blob:none" {
        return Ok(FilterOptions::BlobNone);
    }
    if let Some(v0) = arg.strip_prefix(b"blob:limit=".as_slice()) {
        // A bad magnitude is not its own diagnostic: git falls out of the
        // if/else chain to the generic `invalid filter-spec` at the bottom.
        if let Some(limit) = git_parse_ulong(v0) {
            return Ok(FilterOptions::BlobLimit(limit));
        }
    } else if let Some(v0) = arg.strip_prefix(b"tree:".as_slice()) {
        return match git_parse_ulong(v0) {
            Some(depth) => Ok(FilterOptions::TreeDepth(depth)),
            None => Err("expected 'tree:<depth>'".to_string()),
        };
    } else if let Some(v0) = arg.strip_prefix(b"sparse:oid=".as_slice()) {
        // Any oid name is accepted at parse time; `filter_sparse_oid__init`
        // resolves it when the traversal starts.
        return Ok(FilterOptions::SparseOid(v0.to_vec()));
    } else if arg.strip_prefix(b"sparse:path=".as_slice()).is_some() {
        return Err("sparse:path filters support has been dropped".to_string());
    } else if let Some(v0) = arg.strip_prefix(b"object:type=".as_slice()) {
        // `type_from_string_gently(v0, strlen(v0), 1)`: the four names, exactly.
        return match v0 {
            b"commit" => Ok(FilterOptions::ObjectType(Kind::Commit)),
            b"tree" => Ok(FilterOptions::ObjectType(Kind::Tree)),
            b"blob" => Ok(FilterOptions::ObjectType(Kind::Blob)),
            b"tag" => Ok(FilterOptions::ObjectType(Kind::Tag)),
            _ => Err(format!(
                "'{}' for 'object:type=<type>' is not a valid object type",
                String::from_utf8_lossy(v0)
            )),
        };
    } else if let Some(v0) = arg.strip_prefix(b"combine:".as_slice()) {
        return parse_combine_filter(v0);
    }

    Err(format!(
        "invalid filter-spec '{}'",
        String::from_utf8_lossy(arg)
    ))
}

/// `parse_combine_filter` (list-objects-filter-options.c:180): split on `+`
/// into sub-filters (each of which is parsed recursively), skipping empty
/// segments so a leading or trailing `+` is accepted. An empty body is the one
/// combine-specific error; a body of nothing but `+` is a combine of no
/// sub-filters at all.
fn parse_combine_filter(arg: &[u8]) -> Result<FilterOptions, String> {
    if arg.is_empty() {
        return Err("expected something after combine:".to_string());
    }
    let mut subs = Vec::new();
    let mut p = arg;
    while !p.is_empty() {
        let end = p.iter().position(|&c| c == b'+').unwrap_or(p.len());
        let sub = &p[..end];
        if !sub.is_empty() {
            subs.push(parse_combine_subfilter(sub)?);
        }
        if end == p.len() {
            break;
        }
        p = &p[end + 1..];
    }
    Ok(FilterOptions::Combine(subs))
}

/// `parse_combine_subfilter` (list-objects-filter-options.c:145):
/// percent-decode the segment, reject any reserved character in the *raw*
/// segment, then parse the decoded bytes recursively with a freshly initialised
/// (`allow_auto_filter = 0`) options struct. The `LOFC_AUTO` check git runs
/// afterwards is therefore unreachable: a bare `auto` sub-filter is refused by
/// the recursive parse first.
fn parse_combine_subfilter(subspec: &[u8]) -> Result<FilterOptions, String> {
    let decoded = url_percent_decode(subspec);
    if let Some(c) = has_reserved_character(subspec) {
        return Err(format!("must escape char in sub-filter-spec: '{c}'"));
    }
    match gently_parse_list_objects_filter(&decoded, false)? {
        FilterOptions::Auto => Err("an 'auto' filter cannot be combined".to_string()),
        sub => Ok(sub),
    }
}

/// `parse_list_objects_filter` (list-objects-filter-options.c:273): the
/// `--filter=<spec>` option callback. The first spec is parsed on its own; every
/// later one turns the options into a `LOFC_COMBINE` (`transform_to_combine_type`)
/// and is appended as one more sub-filter, so `--filter=tree:0 --filter=blob:none`
/// is `combine:tree:0+blob:none`. `Err` is the text git `die()`s with.
pub(crate) fn parse_list_objects_filter(
    options: &mut Option<FilterOptions>,
    arg: &[u8],
    allow_auto_filter: bool,
) -> Result<(), String> {
    let Some(current) = options.take() else {
        *options = Some(gently_parse_list_objects_filter(arg, allow_auto_filter)?);
        return Ok(());
    };
    if current == FilterOptions::Auto {
        return Err("an 'auto' filter is incompatible with any other filter".to_string());
    }
    let mut subs = match current {
        FilterOptions::Combine(subs) => subs,
        single => vec![single],
    };
    // The sub-filter is `list_objects_filter_init()`ed, so `auto` is refused by
    // the parse itself before the explicit `LOFC_AUTO` check could fire.
    let sub = gently_parse_list_objects_filter(arg, false)?;
    if sub == FilterOptions::Auto {
        return Err("an 'auto' filter is incompatible with any other filter".to_string());
    }
    subs.push(sub);
    *options = Some(FilterOptions::Combine(subs));
    Ok(())
}

/// git's `RESERVED_NON_WS` set plus every byte at or below a space: the first
/// such byte in `sub` is the one git names in its escape diagnostic.
fn has_reserved_character(sub: &[u8]) -> Option<char> {
    const RESERVED_NON_WS: &[u8] = br#"~`!@#$^&*()[]{}\;'",<>?"#;
    sub.iter()
        .copied()
        .find(|&c| c <= b' ' || RESERVED_NON_WS.contains(&c))
        .map(|c| c as char)
}

/// `url_percent_decode` (`decode_plus = 0`): decode `%XX` where both digits are
/// hex and the byte is non-zero, and copy every other byte through unchanged —
/// which is exactly how git leaves a truncated or malformed `%` in place.
pub(crate) fn url_percent_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && i + 3 <= s.len() {
            if let (Some(h), Some(l)) = (hexval(s[i + 1]), hexval(s[i + 2])) {
                let byte = (h << 4) | l;
                if byte > 0 {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// One hex digit's value, or `None` — the `hex2chr` half git's decoder uses.
fn hexval(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// git's `git_parse_ulong` (via `git_parse_unsigned`), as `blob:limit=` and
/// `tree:` consume their value: the string must be non-empty and hold no `-`
/// anywhere, a base-0 `strtoumax` must convert at least one digit without
/// overflowing an `unsigned long`, and any trailing unit must be one of
/// `k`/`m`/`g` (either case). `None` is git's "0 return" (rejected value).
///
/// The `unsigned long` ceiling is 64-bit on every target this builds for, so
/// only the multiply can overflow `max`; `checked_mul` stands in for git's
/// `unsigned_mult_overflows` / `> max` pair.
pub(crate) fn git_parse_ulong(value: &[u8]) -> Option<u64> {
    if value.is_empty() || value.contains(&b'-') {
        return None;
    }
    let (val, end) = strtoumax_base0(value)?;
    let factor = unit_factor(end)?;
    val.checked_mul(factor)
}

/// `get_unit_factor`: an empty tail is a factor of one, `k`/`m`/`g` scale by
/// 2^10/2^20/2^30, and anything else is git's `0` (an invalid value).
fn unit_factor(end: &[u8]) -> Option<u64> {
    match end {
        b"" => Some(1),
        b"k" | b"K" => Some(1024),
        b"m" | b"M" => Some(1024 * 1024),
        b"g" | b"G" => Some(1024 * 1024 * 1024),
        _ => None,
    }
}

/// C's `strtoumax(value, &end, 0)` over the prefix git's numeric parser reads:
/// skip leading ASCII whitespace and an optional sign, auto-detect the base
/// (`0x` hex, a leading `0` octal, else decimal), and consume digits. Returns
/// the converted value and the unconsumed tail, or `None` when no digit was
/// converted or the magnitude overflows `u64` (git's `ERANGE`).
///
/// git rejects any `-` before this runs, so the negative branch is defensive
/// only; it wraps the way C would rather than inventing a value.
fn strtoumax_base0(value: &[u8]) -> Option<(u64, &[u8])> {
    let mut i = 0;
    while i < value.len() && value[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut negative = false;
    if i < value.len() && (value[i] == b'+' || value[i] == b'-') {
        negative = value[i] == b'-';
        i += 1;
    }

    let (base, start) = if value.len() > i + 2
        && value[i] == b'0'
        && (value[i + 1] | 0x20) == b'x'
        && value[i + 2].is_ascii_hexdigit()
    {
        (16u64, i + 2)
    } else if i < value.len() && value[i] == b'0' {
        (8u64, i)
    } else {
        (10u64, i)
    };

    let mut j = start;
    let mut val: u64 = 0;
    let mut overflow = false;
    while j < value.len() {
        let Some(d) = hexval(value[j]).map(u64::from).filter(|&d| d < base) else {
            break;
        };
        match val.checked_mul(base).and_then(|v| v.checked_add(d)) {
            Some(v) => val = v,
            None => overflow = true,
        }
        j += 1;
    }
    if j == start || overflow {
        return None;
    }
    if negative {
        val = 0u64.wrapping_sub(val);
    }
    Some((val, &value[j..]))
}

// --- list-objects-filter.c ---------------------------------------------------

/// `LOFR_MARK_SEEN` (list-objects-filter.h:52).
pub(crate) const LOFR_MARK_SEEN: u8 = 1 << 0;
/// `LOFR_DO_SHOW` (list-objects-filter.h:53).
pub(crate) const LOFR_DO_SHOW: u8 = 1 << 1;
/// `LOFR_SKIP_TREE` (list-objects-filter.h:54).
pub(crate) const LOFR_SKIP_TREE: u8 = 1 << 2;

/// `enum list_objects_filter_situation` (list-objects-filter.h:57-63).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Situation {
    Commit,
    Tag,
    BeginTree,
    EndTree,
    Blob,
}

/// A git `struct oidset` used as an omit set: membership plus the order ids
/// first went in, which is what a caller hands to [`crate::oidhash`] to print.
#[derive(Default)]
pub(crate) struct Omits {
    order: Vec<ObjectId>,
    set: HashSet<ObjectId>,
}

impl Omits {
    /// `oidset_insert`: returns whether `id` was already present.
    fn insert(&mut self, id: ObjectId) -> bool {
        if self.set.insert(id) {
            self.order.push(id);
            false
        } else {
            true
        }
    }

    /// `oidset_remove`: returns whether `id` was present.
    fn remove(&mut self, id: &ObjectId) -> bool {
        if !self.set.remove(id) {
            return false;
        }
        self.order.retain(|o| o != id);
        true
    }

    /// The ids, in the order they were (last) inserted.
    pub(crate) fn into_ids(self) -> Vec<ObjectId> {
        self.order
    }
}

/// `enum pattern_match_result` (dir.h:405-410), minus `MATCHED_RECURSIVE`, which
/// only cone-mode lists produce and a blob-loaded list never is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PatternMatch {
    Undecided,
    NotMatched,
    Matched,
}

/// `struct frame` (list-objects-filter.c:359).
#[derive(Clone, Copy)]
struct Frame {
    default_match: PatternMatch,
    child_prov_omit: bool,
}

/// `struct subfilter` (list-objects-filter.c:29).
struct Subfilter {
    filter: Filter,
    seen: HashSet<ObjectId>,
    skip_tree: Option<ObjectId>,
}

/// The `filter_data` of each filter kind.
enum FilterData {
    BlobNone,
    BlobLimit(u64),
    /// `struct filter_trees_depth_data` (list-objects-filter.c:126-139).
    TreeDepth {
        seen_at_depth: HashMap<ObjectId, u64>,
        exclude_depth: u64,
        current_depth: u64,
    },
    /// `struct filter_sparse_data` (list-objects-filter.c:379).
    Sparse {
        patterns: Vec<Pattern>,
        case: Case,
        frames: Vec<Frame>,
    },
    ObjectType(Kind),
    /// `struct combine_filter_data` (list-objects-filter.c:624).
    Combine(Vec<Subfilter>),
}

/// git's `struct filter` (list-objects-filter.c:37).
pub(crate) struct Filter {
    kind: FilterData,
    /// `filter->omits`: collected only when the caller asked for them.
    omits: Option<Omits>,
}

/// The one per-object bit this filter family keeps outside its own data:
/// `FILTER_SHOWN_BUT_REVISIT` (list-objects-filter.c:17-27), a flag on the
/// object itself and therefore shared by every sparse sub-filter.
pub(crate) type ShownButRevisit = HashSet<ObjectId>;

impl Filter {
    /// `list_objects_filter__init` (list-objects-filter.c:773) with the
    /// `filter_*__init` each choice dispatches to. `collect_omits` is a non-NULL
    /// `omitted` set. `Err` is the text of a `die()` an initialiser raises —
    /// only `sparse:oid=` has any.
    pub(crate) fn init(
        repo: &gix::Repository,
        options: &FilterOptions,
        collect_omits: bool,
    ) -> Result<Filter, String> {
        let kind = match options {
            FilterOptions::Auto => {
                return Err("BUG: LOFC_AUTO should have been resolved before initializing the filter".into())
            }
            FilterOptions::BlobNone => FilterData::BlobNone,
            FilterOptions::BlobLimit(n) => FilterData::BlobLimit(*n),
            FilterOptions::TreeDepth(depth) => FilterData::TreeDepth {
                seen_at_depth: HashMap::new(),
                exclude_depth: *depth,
                current_depth: 0,
            },
            FilterOptions::SparseOid(name) => sparse_oid_init(repo, name)?,
            FilterOptions::ObjectType(kind) => FilterData::ObjectType(*kind),
            FilterOptions::Combine(subs) => {
                // `filter_combine__init`: each sub-filter gets its own omit set
                // when the top one collects, merged back in `finish`.
                let mut built = Vec::with_capacity(subs.len());
                for sub in subs {
                    built.push(Subfilter {
                        filter: Filter::init(repo, sub, collect_omits)?,
                        seen: HashSet::new(),
                        skip_tree: None,
                    });
                }
                FilterData::Combine(built)
            }
        };
        Ok(Filter {
            kind,
            omits: collect_omits.then(Omits::default),
        })
    }

    /// `list_objects_filter__free` (list-objects-filter.c:822) as far as it
    /// is observable: `filter_combine__finalize_omits` unions every sub-filter's
    /// omit set into the caller's.
    pub(crate) fn finish(self) -> Omits {
        let mut omits = self.omits.unwrap_or_default();
        if let FilterData::Combine(subs) = self.kind {
            for sub in subs {
                for id in sub.filter.finish().into_ids() {
                    omits.insert(id);
                }
            }
        }
        omits
    }
}

/// `list_objects_filter__filter_object` (list-objects-filter.c:799): only an
/// object carrying `NOT_USER_GIVEN` is offered to the filter; with no filter,
/// or for an object the user named, everything is shown except `LOFS_END_TREE`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn filter_object(
    repo: &gix::Repository,
    filter: Option<&mut Filter>,
    not_user_given: bool,
    situation: Situation,
    id: ObjectId,
    pathname: &[u8],
    filename: &[u8],
    revisit: &mut ShownButRevisit,
) -> u8 {
    match filter {
        Some(filter) if not_user_given => {
            filter.filter_object_fn(repo, situation, id, pathname, filename, revisit)
        }
        _ if situation == Situation::EndTree => 0,
        _ => LOFR_MARK_SEEN | LOFR_DO_SHOW,
    }
}

impl Filter {
    fn filter_object_fn(
        &mut self,
        repo: &gix::Repository,
        situation: Situation,
        id: ObjectId,
        pathname: &[u8],
        filename: &[u8],
        revisit: &mut ShownButRevisit,
    ) -> u8 {
        let omits = self.omits.as_mut();
        match &mut self.kind {
            FilterData::BlobNone => filter_blobs_none(situation, id, omits),
            FilterData::BlobLimit(max_bytes) => filter_blobs_limit(repo, situation, id, omits, *max_bytes),
            FilterData::TreeDepth {
                seen_at_depth,
                exclude_depth,
                current_depth,
            } => filter_trees_depth(situation, id, omits, seen_at_depth, *exclude_depth, current_depth),
            FilterData::Sparse { patterns, case, frames } => {
                filter_sparse(situation, id, pathname, filename, omits, patterns, *case, frames, revisit)
            }
            FilterData::ObjectType(kind) => filter_object_type(situation, *kind),
            FilterData::Combine(subs) => filter_combine(repo, situation, id, pathname, filename, subs, revisit),
        }
    }
}

/// `filter_blobs_none` (list-objects-filter.c:72).
fn filter_blobs_none(situation: Situation, id: ObjectId, omits: Option<&mut Omits>) -> u8 {
    match situation {
        Situation::Tag | Situation::Commit | Situation::BeginTree => LOFR_MARK_SEEN | LOFR_DO_SHOW,
        Situation::EndTree => 0,
        Situation::Blob => {
            if let Some(omits) = omits {
                omits.insert(id);
            }
            // but not LOFR_DO_SHOW (hard omit)
            LOFR_MARK_SEEN
        }
    }
}

/// `filter_trees_update_omits` (list-objects-filter.c:147): returns whether
/// the id was in the omit set before.
fn filter_trees_update_omits(id: ObjectId, omits: Option<&mut Omits>, include_it: bool) -> bool {
    let Some(omits) = omits else { return false };
    if include_it {
        omits.remove(&id)
    } else {
        omits.insert(id)
    }
}

/// `filter_trees_depth` (list-objects-filter.c:161). Trees are never marked
/// seen, so a tree reached again at a shallower depth is walked again.
fn filter_trees_depth(
    situation: Situation,
    id: ObjectId,
    mut omits: Option<&mut Omits>,
    seen_at_depth: &mut HashMap<ObjectId, u64>,
    exclude_depth: u64,
    current_depth: &mut u64,
) -> u8 {
    let include_it = *current_depth < exclude_depth;
    match situation {
        Situation::Tag | Situation::Commit => LOFR_MARK_SEEN | LOFR_DO_SHOW,
        Situation::EndTree => {
            *current_depth -= 1;
            0
        }
        Situation::Blob => {
            filter_trees_update_omits(id, omits, include_it);
            if include_it {
                LOFR_MARK_SEEN | LOFR_DO_SHOW
            } else {
                0
            }
        }
        Situation::BeginTree => {
            let already_seen = match seen_at_depth.get(&id) {
                None => {
                    seen_at_depth.insert(id, *current_depth);
                    false
                }
                Some(depth) => *current_depth >= *depth,
            };
            let filter_res = if already_seen {
                LOFR_SKIP_TREE
            } else {
                let collecting = omits.is_some();
                let been_omitted = filter_trees_update_omits(id, omits.as_deref_mut(), include_it);
                seen_at_depth.insert(id, *current_depth);
                if include_it {
                    LOFR_DO_SHOW
                } else if collecting && !been_omitted {
                    // Must update omit information of children recursively;
                    // they have not been omitted yet.
                    0
                } else {
                    LOFR_SKIP_TREE
                }
            };
            *current_depth += 1;
            filter_res
        }
    }
}

/// `filter_blobs_limit` (list-objects-filter.c:273).
fn filter_blobs_limit(
    repo: &gix::Repository,
    situation: Situation,
    id: ObjectId,
    omits: Option<&mut Omits>,
    max_bytes: u64,
) -> u8 {
    match situation {
        Situation::Tag | Situation::Commit | Situation::BeginTree => LOFR_MARK_SEEN | LOFR_DO_SHOW,
        Situation::EndTree => 0,
        Situation::Blob => {
            // A blob we do not have locally cannot be measured: be conservative
            // and force show it, letting the caller deal with the ambiguity.
            let include_it = match repo.find_header(id) {
                Ok(header) if header.kind() == Kind::Blob => header.size() < max_bytes,
                _ => true,
            };
            match omits {
                Some(omits) if include_it => {
                    omits.remove(&id);
                }
                Some(omits) => {
                    omits.insert(id);
                }
                None => {}
            }
            if include_it {
                LOFR_MARK_SEEN | LOFR_DO_SHOW
            } else {
                // but not LOFR_DO_SHOW (hard omit)
                LOFR_MARK_SEEN
            }
        }
    }
}

/// `PATTERN_MAX_FILE_SIZE` (dir.h): 100 MiB.
const PATTERN_MAX_FILE_SIZE: usize = 100 * 1024 * 1024;

/// `filter_sparse_oid__init` (list-objects-filter.c:522).
fn sparse_oid_init(repo: &gix::Repository, name: &[u8]) -> Result<FilterData, String> {
    let shown = String::from_utf8_lossy(name);
    // `repo_get_oid_with_flags(..., GET_OID_BLOB)`: the flag only steers
    // disambiguation, so any object the name resolves to is accepted here and
    // a non-blob is rejected by the read below.
    let oid = match repo.rev_parse_single(name.as_bstr()) {
        Ok(id) => id.detach(),
        Err(_) => return Err(format!("unable to access sparse blob in '{shown}'")),
    };
    // `add_patterns_from_blob_to_list` → `do_read_blob`: a missing object or a
    // non-blob is -1, which is the "unable to parse" die.
    let unparsable = || format!("unable to parse sparse filter data in {oid}");
    let object = repo.find_object(oid).map_err(|_| unparsable())?;
    if object.kind != Kind::Blob {
        return Err(unparsable());
    }
    let mut buf = object.data.clone();
    let patterns = if buf.is_empty() {
        // `do_read_blob` returns 0 for an empty blob, and the list stays empty.
        Vec::new()
    } else {
        if buf.last() != Some(&b'\n') {
            buf.push(b'\n');
        }
        if buf.len() > PATTERN_MAX_FILE_SIZE {
            eprintln!("warning: ignoring excessively large pattern blob: {oid}");
            return Err(unparsable());
        }
        add_patterns_from_buffer(&buf)
    };
    let case = match repo.config_snapshot().boolean("core.ignorecase") {
        Some(true) => Case::Fold,
        _ => Case::Sensitive,
    };
    Ok(FilterData::Sparse {
        patterns,
        case,
        // `default_match = 0`: NOT_MATCHED, whatever the comment beside it says.
        frames: vec![Frame {
            default_match: PatternMatch::NotMatched,
            child_prov_omit: false,
        }],
    })
}

/// `add_patterns_from_buffer` (dir.c:1226-1253): skip a UTF-8 BOM, then one
/// pattern per `\n`-terminated line, ignoring empty lines and `#` comments,
/// dropping a `\r` before the newline and unescaped trailing spaces.
fn add_patterns_from_buffer(buf: &[u8]) -> Vec<Pattern> {
    let buf = buf.strip_prefix(b"\xef\xbb\xbf".as_slice()).unwrap_or(buf);
    let mut patterns = Vec::new();
    let mut entry = 0;
    for i in 0..buf.len() {
        if buf[i] != b'\n' {
            continue;
        }
        if entry != i && buf[entry] != b'#' {
            let mut line = &buf[entry..i];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            let line = trim_trailing_spaces(line);
            // `add_pattern` → `parse_path_pattern`; an empty result matches
            // nothing, which gix's `None` expresses by leaving it out.
            if let Some(p) = Pattern::from_bytes(line) {
                patterns.push(p);
            }
        }
        entry = i + 1;
    }
    patterns
}

/// `trim_trailing_spaces` (dir.c:1029-1050): cut a run of spaces at the end,
/// unless the first of them is escaped by a backslash.
fn trim_trailing_spaces(buf: &[u8]) -> &[u8] {
    let mut last_space: Option<usize> = None;
    let mut p = 0;
    while p < buf.len() {
        match buf[p] {
            b' ' => {
                if last_space.is_none() {
                    last_space = Some(p);
                }
            }
            b'\\' => {
                p += 1;
                if p >= buf.len() {
                    return buf;
                }
                last_space = None;
            }
            _ => last_space = None,
        }
        p += 1;
    }
    match last_space {
        Some(at) => &buf[..at],
        None => buf,
    }
}

/// `path_matches_pattern_list` (dir.c:1477-1497) for a non-cone list: the last
/// pattern that matches decides (`last_matching_pattern_from_list`)
/// through the same gix matcher `sparse-checkout` uses.
fn path_matches_pattern_list(
    pathname: &[u8],
    filename: &[u8],
    is_dir: bool,
    patterns: &[Pattern],
    case: Case,
) -> PatternMatch {
    let basename = pathname.len() - filename.len();
    let hit = patterns.iter().rev().find(|p| {
        p.matches_repo_relative_path(
            BStr::new(pathname),
            (basename != 0).then_some(basename),
            Some(is_dir),
            case,
            WildMode::NO_MATCH_SLASH_LITERAL,
        )
    });
    match hit {
        Some(p) if p.is_negative() => PatternMatch::NotMatched,
        Some(_) => PatternMatch::Matched,
        None => PatternMatch::Undecided,
    }
}

/// `filter_sparse` (list-objects-filter.c:386).
#[allow(clippy::too_many_arguments)]
fn filter_sparse(
    situation: Situation,
    id: ObjectId,
    pathname: &[u8],
    filename: &[u8],
    omits: Option<&mut Omits>,
    patterns: &[Pattern],
    case: Case,
    frames: &mut Vec<Frame>,
    revisit: &mut ShownButRevisit,
) -> u8 {
    match situation {
        Situation::Tag | Situation::Commit => LOFR_MARK_SEEN | LOFR_DO_SHOW,
        Situation::BeginTree => {
            let mut matched = path_matches_pattern_list(pathname, filename, true, patterns, case);
            if matched == PatternMatch::Undecided {
                matched = frames[frames.len() - 1].default_match;
            }
            frames.push(Frame {
                default_match: matched,
                child_prov_omit: false,
            });
            // A tree with this id may appear at several paths, so it is not
            // marked seen yet; it is shown only the first time it is visited.
            if !revisit.insert(id) {
                return 0;
            }
            LOFR_DO_SHOW
        }
        Situation::EndTree => {
            let frame = frames.pop().expect("BEGIN_TREE pushed a frame");
            let parent = frames.len() - 1;
            frames[parent].child_prov_omit |= frame.child_prov_omit;
            // Nothing below was provisionally omitted: the tree need not be
            // revisited.
            if !frame.child_prov_omit {
                return LOFR_MARK_SEEN;
            }
            0
        }
        Situation::Blob => {
            let top = frames.len() - 1;
            let mut matched = path_matches_pattern_list(pathname, filename, false, patterns, case);
            if matched == PatternMatch::Undecided {
                matched = frames[top].default_match;
            }
            if matched == PatternMatch::Matched {
                if let Some(omits) = omits {
                    omits.remove(&id);
                }
                return LOFR_MARK_SEEN | LOFR_DO_SHOW;
            }
            // Provisionally omit it: the same blob may be reachable through a
            // path that does match, so leave it unmarked to be asked again.
            if let Some(omits) = omits {
                omits.insert(id);
            }
            frames[top].child_prov_omit = true;
            0
        }
    }
}

/// `filter_object_type` (list-objects-filter.c:556).
fn filter_object_type(situation: Situation, object_type: Kind) -> u8 {
    let only = |kind: Kind| match object_type == kind {
        true => LOFR_MARK_SEEN | LOFR_DO_SHOW,
        false => LOFR_MARK_SEEN,
    };
    match situation {
        Situation::Tag => only(Kind::Tag),
        Situation::Commit => only(Kind::Commit),
        Situation::BeginTree => {
            // Only commits or tags wanted: no need to walk down trees.
            if matches!(object_type, Kind::Commit | Kind::Tag) {
                return LOFR_SKIP_TREE;
            }
            only(Kind::Tree)
        }
        Situation::Blob => only(Kind::Blob),
        Situation::EndTree => 0,
    }
}

/// `process_subfilter` (list-objects-filter.c:629).
fn process_subfilter(
    repo: &gix::Repository,
    situation: Situation,
    id: ObjectId,
    pathname: &[u8],
    filename: &[u8],
    sub: &mut Subfilter,
    revisit: &mut ShownButRevisit,
) -> u8 {
    if let Some(skip_tree) = sub.skip_tree {
        if situation == Situation::EndTree && id == skip_tree {
            sub.skip_tree = None;
        } else {
            return 0;
        }
    }
    if sub.seen.contains(&id) {
        return 0;
    }
    // The sub-filter sees the same object flags, so `NOT_USER_GIVEN` holds: the
    // combine filter was only consulted because it was set.
    let result = filter_object(repo, Some(&mut sub.filter), true, situation, id, pathname, filename, revisit);
    if result & LOFR_MARK_SEEN != 0 {
        sub.seen.insert(id);
    }
    if result & LOFR_SKIP_TREE != 0 {
        sub.skip_tree = Some(id);
    }
    result
}

/// `filter_combine` (list-objects-filter.c:672): shown only if every
/// sub-filter shows it, marked seen only if every one marks it, and a tree is
/// skipped only while every sub-filter is skipping.
fn filter_combine(
    repo: &gix::Repository,
    situation: Situation,
    id: ObjectId,
    pathname: &[u8],
    filename: &[u8],
    subs: &mut [Subfilter],
    revisit: &mut ShownButRevisit,
) -> u8 {
    let mut combined = LOFR_DO_SHOW | LOFR_MARK_SEEN | LOFR_SKIP_TREE;
    for sub in subs.iter_mut() {
        let sub_result = process_subfilter(repo, situation, id, pathname, filename, sub, revisit);
        if sub_result & LOFR_DO_SHOW == 0 {
            combined &= !LOFR_DO_SHOW;
        }
        if sub_result & LOFR_MARK_SEEN == 0 {
            combined &= !LOFR_MARK_SEEN;
        }
        if sub.skip_tree.is_none() {
            combined &= !LOFR_SKIP_TREE;
        }
    }
    combined
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(specs: &[&str]) -> Result<Option<FilterOptions>, String> {
        let mut options = None;
        for spec in specs {
            parse_list_objects_filter(&mut options, spec.as_bytes(), false)?;
        }
        Ok(options)
    }

    #[test]
    fn repeated_filters_become_one_combine() {
        assert_eq!(
            parse_all(&["tree:0", "blob:none"]),
            Ok(Some(FilterOptions::Combine(vec![
                FilterOptions::TreeDepth(0),
                FilterOptions::BlobNone,
            ])))
        );
        // An explicit combine is extended rather than nested.
        assert_eq!(
            parse_all(&["combine:blob:none+tree:2", "object:type=tree"]),
            Ok(Some(FilterOptions::Combine(vec![
                FilterOptions::BlobNone,
                FilterOptions::TreeDepth(2),
                FilterOptions::ObjectType(Kind::Tree),
            ])))
        );
    }

    #[test]
    fn sub_filter_errors_name_the_sub_filter() {
        assert_eq!(
            parse_all(&["combine:blob:none+bogus:spec"]),
            Err("invalid filter-spec 'bogus:spec'".to_string())
        );
        assert_eq!(
            parse_all(&["blob:none", "object:type=bogus"]),
            Err("'bogus' for 'object:type=<type>' is not a valid object type".to_string())
        );
        assert_eq!(parse_all(&["combine:+"]), Ok(Some(FilterOptions::Combine(Vec::new()))));
    }

    #[test]
    fn trailing_spaces_are_trimmed_unless_escaped() {
        assert_eq!(trim_trailing_spaces(b"a  "), b"a");
        assert_eq!(trim_trailing_spaces(b"a\\  "), b"a\\ ");
        assert_eq!(trim_trailing_spaces(b"a\\"), b"a\\");
    }
}
