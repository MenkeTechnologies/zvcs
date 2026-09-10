//! The server half of the shallow protocol: turning a client's `deepen*` request
//! into the boundary it has to record.
//!
//! `upload-pack` is the only caller. Both wire protocols ask the same question in
//! the same words — `shallow <oid>` lines for what the client already treats as a
//! cutoff, then `deepen <n>` / `deepen-since <ts>` / `deepen-not <ref>` /
//! `deepen-relative` for how much further it wants to see — and both expect the
//! same three answers back: which commits become the new cutoff (`shallow`), which
//! of the client's old cutoffs stop being one (`unshallow`), and a pack holding
//! exactly the commits inside the window.
//!
//! Ported from `shallow.c`'s `get_shallow_commits()` and
//! `get_shallow_commits_by_rev_list()`, plus `upload-pack.c`'s `deepen()`,
//! `send_shallow()` and `send_unshallow()`. The two boundary rules are different
//! and the difference is observable, so both are kept:
//!
//!   * **`deepen <n>`** counts hops. The wants sit at depth 1, and a commit that
//!     reaches depth `n` is a boundary *whether or not it has parents* — which is
//!     why `--depth 5` against a five-commit history still writes a `.git/shallow`
//!     naming the root, while `--depth 9` against the same history writes none.
//!   * **`deepen-since` / `deepen-not`** cut by predicate. Every commit that
//!     passes is kept, and a kept commit becomes a boundary only when it has a
//!     parent that did not pass.
//!
//! `deepen-relative` is not a third rule. `get_shallow_commits()`
//! (shallow.c:243-256) turns it into the first one before any boundary is
//! computed: it measures how far below the wants the client's own cutoff sits and
//! adds that to the requested count, so the walk that follows is an ordinary
//! `deepen <n>` from the same tips. A request with no cutoff to measure from —
//! a client that is not shallow at all, or whose cutoffs this fetch's wants
//! cannot reach — returns NULL without walking, which is silence on the wire and
//! an uncut pack rather than a boundary at the wants.
//!
//! A repository that is itself shallow contributes its own grafts to both rules:
//! its cutoff commits are parentless as far as this walk is concerned, and they
//! are boundaries in their own right, because a client cannot be told to expect
//! parents this server does not have.

use gix::ObjectId;
use std::collections::{HashMap, HashSet, VecDeque};

/// How much further the client wants to see, as the request lines describe it.
/// All-`None` means the client sent no `deepen*` line at all, which is the
/// ordinary non-shallow fetch.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Deepen {
    /// `deepen <n>`: how many commits of history to keep, counting the wants.
    pub depth: Option<u32>,
    /// `deepen-since <ts>`: keep commits committed at or after this epoch second.
    pub since: Option<i64>,
    /// `deepen-not <ref>`: keep commits not reachable from these.
    pub not: Vec<String>,
    /// `deepen-relative`: `depth` counts from the client's current boundary
    /// rather than from the wants.
    pub relative: bool,
}

impl Deepen {
    /// Whether any boundary has to be computed at all — git's
    /// `data->depth || data->deepen_rev_list`, the condition that decides between
    /// answering with shallow lines and just registering what the client sent.
    pub fn requested(&self) -> bool {
        self.depth.is_some() || self.since.is_some() || !self.not.is_empty()
    }
}

/// One request's shallow state: what the client already has as a cutoff, and how
/// much further it asked to go.
#[derive(Debug, Default, Clone)]
pub struct Request {
    /// `shallow <oid>` lines — git's `data->shallows`, which are *not* registered
    /// as grafts while deepening, since this server has the real parents.
    pub client_shallow: Vec<ObjectId>,
    pub deepen: Deepen,
}

impl Request {
    /// Absorb one request line, returning whether it was a shallow-protocol line.
    /// A malformed value is an `Err` carrying git's message for it.
    ///
    /// `receive_needs()` (v0) and `process_args()` (v2) parse the identical five
    /// tokens, so both callers share this.
    pub fn absorb(&mut self, line: &str) -> Result<bool, String> {
        if let Some(hex) = line.strip_prefix("shallow ") {
            let id = ObjectId::from_hex(hex.trim().as_bytes())
                .map_err(|_| format!("invalid shallow line: {line}"))?;
            if !self.client_shallow.contains(&id) {
                self.client_shallow.push(id);
            }
            return Ok(true);
        }
        if let Some(n) = line.strip_prefix("deepen ") {
            let depth: i64 = n
                .trim()
                .parse()
                .map_err(|_| format!("invalid deepen: {line}"))?;
            // `receive_needs()`: a non-positive depth is a protocol error, and a
            // depth of `INFINITE_DEPTH` means "no limit", i.e. `--unshallow`.
            if depth <= 0 {
                return Err(format!("Invalid deepen: {line}"));
            }
            self.deepen.depth = Some(depth as u32);
            return Ok(true);
        }
        if let Some(ts) = line.strip_prefix("deepen-since ") {
            let secs: i64 = ts
                .trim()
                .parse()
                .map_err(|_| format!("Invalid deepen-since: {line}"))?;
            self.deepen.since = Some(secs);
            return Ok(true);
        }
        if let Some(name) = line.strip_prefix("deepen-not ") {
            self.deepen.not.push(name.trim().to_owned());
            return Ok(true);
        }
        if line.trim_end() == "deepen-relative" {
            self.deepen.relative = true;
            return Ok(true);
        }
        Ok(false)
    }
}

/// What the client has to be told, and what the pack has to carry.
#[derive(Debug, Default)]
pub struct Boundary {
    /// `shallow <oid>` lines: the new cutoff, minus anything the client already
    /// listed as one (`send_shallow()` skips `CLIENT_SHALLOW`).
    pub shallow: Vec<ObjectId>,
    /// `unshallow <oid>` lines: cutoffs of the client's that this pack fills in.
    pub unshallow: Vec<ObjectId>,
    /// Every commit inside the window, boundary included — the pack's commit set.
    pub commits: Vec<ObjectId>,
    /// Whether a window was computed at all, and so whether the pack is cut to
    /// [`Self::commits`].
    ///
    /// `get_shallow_commits()` (shallow.c:243-256) answers a `deepen-relative`
    /// request with NULL *before walking* when there is no cutoff of the client's
    /// below the wants to measure from. Nothing is flagged, so `send_shallow()`
    /// and `send_unshallow()` both write nothing and the pack `upload-pack` goes
    /// on to build is an ordinary one — bounded by the client's grafts, not by a
    /// window this walk never produced.
    pub windowed: bool,
}

/// A repository's own grafts, as a set. A commit in here is walked as if it had
/// no parents, because this server does not have them either.
fn server_grafts(repo: &gix::Repository) -> HashSet<ObjectId> {
    repo.shallow_commits()
        .ok()
        .flatten()
        .map(|c| c.iter().copied().collect())
        .unwrap_or_default()
}

/// The parents this server can actually serve for `id`: none at a graft, and
/// none for anything that is not a commit.
fn parents_of(repo: &gix::Repository, grafts: &HashSet<ObjectId>, id: ObjectId) -> Vec<ObjectId> {
    if grafts.contains(&id) {
        return Vec::new();
    }
    match repo.find_commit(id) {
        Ok(commit) => commit.parent_ids().map(|p| p.detach()).collect(),
        Err(_) => Vec::new(),
    }
}

/// `parse_object()` followed by `deref_tag()`: a `want` may name a tag, and the
/// walk needs the commit under it.
fn peel_to_commit(repo: &gix::Repository, id: ObjectId) -> Option<ObjectId> {
    let object = repo.find_object(id).ok()?;
    object.peel_to_kind(gix::objs::Kind::Commit).ok().map(|c| c.id)
}

/// One depth walk's answer.
struct DepthWalk {
    /// Every commit the walk reached, boundary included, in visit order.
    visited: Vec<ObjectId>,
    /// The ones the walk stopped at.
    boundary: HashSet<ObjectId>,
    /// The shallowest depth each visited commit was reached at, which is the
    /// `commit_depth` slab `get_shallows_or_depth()` carries alongside its walk.
    depth_of: HashMap<ObjectId, u32>,
}

/// `get_shallows_or_depth()` (shallow.c:139-232) in its boundary mode: breadth-first
/// from `tips`, cutting at `depth` hops. The tips are depth 1, so `depth == 1`
/// makes every tip a boundary and fetches nothing behind them.
fn walk_by_depth(
    repo: &gix::Repository,
    tips: &[ObjectId],
    depth: u32,
    grafts: &HashSet<ObjectId>,
) -> DepthWalk {
    let mut visited: Vec<ObjectId> = Vec::new();
    let mut boundary: HashSet<ObjectId> = HashSet::new();
    // The shallowest depth each commit was reached at; a later, deeper arrival
    // never overrides it, which is what keeps a merge's shared ancestor at the
    // depth of its nearest path.
    let mut seen: HashMap<ObjectId, u32> = HashMap::new();
    let mut queue: VecDeque<(ObjectId, u32)> = tips.iter().map(|id| (*id, 1)).collect();

    while let Some((id, cur_depth)) = queue.pop_front() {
        match seen.get(&id) {
            Some(prev) if *prev <= cur_depth => continue,
            None => visited.push(id),
            _ => {}
        }
        seen.insert(id, cur_depth);

        // The two boundary conditions, in git's order: the depth ran out, or this
        // server has no parents to offer.
        if cur_depth >= depth || grafts.contains(&id) {
            boundary.insert(id);
            continue;
        }
        boundary.remove(&id);
        for parent in parents_of(repo, grafts, id) {
            queue.push_back((parent, cur_depth.saturating_add(1)));
        }
    }
    DepthWalk { visited, boundary, depth_of: seen }
}

/// `get_shallows_depth()` (shallow.c:234-241): how far below the wants the
/// client's own cutoff sits, which is what a `deepen-relative` request counts
/// from.
///
/// `get_shallows_or_depth()`'s `shallows` mode runs the same walk with no depth
/// limit — it stops only where this server's own grafts do — and keeps
/// `cur_depth_shallow`, the *shallowest* depth any of the client's `shallow`
/// commits was reached at. The tips are depth 1, so a client cutoff sitting at a
/// want answers 1. Zero is the "not reachable at all" answer, and it is the one
/// [`compute`] refuses on.
fn shallows_depth(
    repo: &gix::Repository,
    tips: &[ObjectId],
    grafts: &HashSet<ObjectId>,
    client_shallow: &[ObjectId],
) -> u32 {
    if client_shallow.is_empty() {
        return 0;
    }
    let depths = walk_by_depth(repo, tips, u32::MAX, grafts).depth_of;
    client_shallow.iter().filter_map(|id| depths.get(id).copied()).min().unwrap_or(0)
}

/// `get_shallow_commits_by_rev_list()` (shallow.c:180-227): keep every commit the
/// predicate accepts, and call a kept commit a boundary when a parent of it was
/// rejected. Unlike the depth walk, a kept root commit is *not* a boundary — there
/// is nothing behind it to promise.
fn walk_by_predicate(
    repo: &gix::Repository,
    tips: &[ObjectId],
    grafts: &HashSet<ObjectId>,
    mut keep: impl FnMut(&gix::Repository, ObjectId) -> bool,
) -> (Vec<ObjectId>, HashSet<ObjectId>) {
    let mut visited: Vec<ObjectId> = Vec::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut queue: VecDeque<ObjectId> = VecDeque::new();
    for id in tips {
        if keep(repo, *id) && seen.insert(*id) {
            visited.push(*id);
            queue.push_back(*id);
        }
    }
    let mut boundary: HashSet<ObjectId> = HashSet::new();
    let mut pending: Vec<(ObjectId, Vec<ObjectId>)> = Vec::new();

    while let Some(id) = queue.pop_front() {
        let parents = parents_of(repo, grafts, id);
        if grafts.contains(&id) {
            boundary.insert(id);
        }
        for parent in &parents {
            if !keep(repo, *parent) {
                continue;
            }
            if seen.insert(*parent) {
                visited.push(*parent);
                queue.push_back(*parent);
            }
        }
        pending.push((id, parents));
    }
    let inside: HashSet<ObjectId> = seen;
    for (id, parents) in pending {
        if parents.iter().any(|p| !inside.contains(p)) {
            boundary.insert(id);
        }
    }
    (visited, boundary)
}

/// `deepen()` (upload-pack.c:667-716): compute the window, then work out which
/// lines the client needs from the difference between the new boundary and the one
/// it declared.
pub fn compute(repo: &gix::Repository, wants: &[ObjectId], request: &Request) -> Boundary {
    let grafts = server_grafts(repo);
    let tips: Vec<ObjectId> = wants.iter().filter_map(|id| peel_to_commit(repo, *id)).collect();
    if tips.is_empty() {
        return Boundary::default();
    }

    let (visited, boundary) = if let Some(depth) = request.deepen.depth {
        // `get_shallow_commits()` (shallow.c:243-256): `deepen-relative` is folded
        // into the absolute depth before the boundary walk begins. The client's own
        // cutoff is located below the wants and its depth is *added* to the
        // requested count, so what runs is the same hop count from the same tips as
        // a plain `deepen <n>` — not a second walk starting at the cutoff. The
        // difference shows whenever the client has more than one cutoff at more than
        // one depth: git measures the shallowest of them once and applies a single
        // absolute cutoff, so a deeper cutoff is simply passed and unshallowed.
        let depth = if request.deepen.relative {
            match shallows_depth(repo, &tips, &grafts, &request.client_shallow) {
                // "else return NULL": no cutoff below the wants to count from —
                // a client that is not shallow at all sends no `shallow` line and
                // lands here — so there is nothing to say and nothing to cut.
                0 => return Boundary::default(),
                cur_shallow_depth => depth.saturating_add(cur_shallow_depth),
            }
        } else {
            depth
        };
        let walk = walk_by_depth(repo, &tips, depth, &grafts);
        (walk.visited, walk.boundary)
    } else {
        let since = request.deepen.since;
        let excluded = ancestors_of_refs(repo, &request.deepen.not, &grafts);
        walk_by_predicate(repo, &tips, &grafts, move |repo, id| {
            if excluded.contains(&id) {
                return false;
            }
            match since {
                None => true,
                Some(since) => repo
                    .find_commit(id)
                    .ok()
                    .and_then(|c| c.time().ok())
                    .map(|t| t.seconds >= since)
                    .unwrap_or(false),
            }
        })
    };

    let client_shallow: HashSet<ObjectId> = request.client_shallow.iter().copied().collect();
    let inside: HashSet<ObjectId> = visited.iter().copied().collect();

    // `send_shallow()`: a boundary the client already records needs no line.
    let shallow: Vec<ObjectId> = visited
        .iter()
        .copied()
        .filter(|id| boundary.contains(id) && !client_shallow.contains(id))
        .collect();
    // `send_unshallow()`: a cutoff of the client's that this walk went past.
    let unshallow: Vec<ObjectId> = request
        .client_shallow
        .iter()
        .copied()
        .filter(|id| inside.contains(id) && !boundary.contains(id))
        .collect();

    Boundary { shallow, unshallow, commits: visited, windowed: true }
}

/// The `^<ref>` half of a `deepen-not` request: every commit reachable from the
/// named refs, which is what the window must exclude. A name that does not resolve
/// contributes nothing, matching git's tolerance for a `deepen-not` naming a ref
/// the server does not have.
fn ancestors_of_refs(
    repo: &gix::Repository,
    names: &[String],
    grafts: &HashSet<ObjectId>,
) -> HashSet<ObjectId> {
    let mut tips: Vec<ObjectId> = Vec::new();
    for name in names {
        let resolved = repo
            .rev_parse_single(name.as_str())
            .ok()
            .map(|id| id.detach())
            .or_else(|| ObjectId::from_hex(name.as_bytes()).ok());
        if let Some(id) = resolved.and_then(|id| peel_to_commit(repo, id)) {
            tips.push(id);
        }
    }
    if tips.is_empty() {
        return HashSet::new();
    }
    walk_by_depth(repo, &tips, u32::MAX, grafts).visited.into_iter().collect()
}

/// The commits the client can be assumed to hold, given what it said it `have`s
/// and where its own boundary is: the walk stops at a client cutoff, because the
/// client has no parents behind one.
pub fn client_side_commits(
    repo: &gix::Repository,
    haves: &[ObjectId],
    client_shallow: &[ObjectId],
) -> Vec<ObjectId> {
    let mut grafts = server_grafts(repo);
    grafts.extend(client_shallow.iter().copied());
    let tips: Vec<ObjectId> = haves.iter().filter_map(|id| peel_to_commit(repo, *id)).collect();
    if tips.is_empty() {
        return Vec::new();
    }
    walk_by_depth(repo, &tips, u32::MAX, &grafts).visited
}

/// The pack for a shallow request: everything the window's commits name, minus
/// everything the client's own bounded history already names.
pub fn objects_within(
    repo: &gix::Repository,
    wants: &[ObjectId],
    window: &[ObjectId],
    haves: &[ObjectId],
    client_shallow: &[ObjectId],
) -> Vec<ObjectId> {
    // The wants ride along unpeeled so a `want` naming a tag object packs the tag
    // itself, as `reachable_objects` does for the ordinary path.
    let mut roots: Vec<ObjectId> = window.to_vec();
    roots.extend(wants.iter().copied().filter(|id| !window.contains(id)));
    // Ordered, because this list *is* the pack: `pack_bytes_with_summary` writes the
    // entries in the order it is handed them, so a `HashSet`'s iteration order became
    // the pack's, and an unspecified order made every shallow clone of the same
    // repository produce a different pack. Measured before the change:
    // `clone --bare --no-local --depth=1 . c.git` run four times wrote four pack names
    // (6ca62d08…, 4e031ab3…, 92619911…, 316c0ba7…) where stock wrote one
    // (b8c43e57…) twice. `objects_to_send()` had the same defect on the non-shallow
    // path and already carries the ordered form; this is the shallow half of it.
    let want_closure = crate::porcelain::push_proto::expand_roots_ordered(repo, &roots);

    let client = client_side_commits(repo, haves, client_shallow);
    if client.is_empty() {
        return want_closure;
    }
    // The `have` side is only ever asked "does it contain this", so it stays hashed.
    let have_closure = crate::porcelain::push_proto::expand_roots(repo, &client);
    want_closure.into_iter().filter(|id| !have_closure.contains(id)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absorb_reads_every_shallow_token() {
        let mut req = Request::default();
        let id = "0123456789012345678901234567890123456789";
        assert!(req.absorb(&format!("shallow {id}")).unwrap());
        assert!(req.absorb("deepen 3").unwrap());
        assert!(req.absorb("deepen-since 1700000000").unwrap());
        assert!(req.absorb("deepen-not refs/tags/v1").unwrap());
        assert!(req.absorb("deepen-relative").unwrap());
        assert!(!req.absorb("want abc").unwrap());

        assert_eq!(req.client_shallow, vec![ObjectId::from_hex(id.as_bytes()).unwrap()]);
        assert_eq!(req.deepen.depth, Some(3));
        assert_eq!(req.deepen.since, Some(1_700_000_000));
        assert_eq!(req.deepen.not, vec!["refs/tags/v1".to_string()]);
        assert!(req.deepen.relative);
        assert!(req.deepen.requested());
    }

    /// `receive_needs()` refuses a depth that cannot describe a window.
    #[test]
    fn absorb_rejects_non_positive_depth() {
        let mut req = Request::default();
        assert!(req.absorb("deepen 0").is_err());
        assert!(req.absorb("deepen -2").is_err());
        assert!(req.absorb("deepen x").is_err());
        assert!(!req.deepen.requested());
    }

    /// A request with only `shallow` lines is not a deepening request: the server
    /// registers the grafts and sends no shallow-info.
    #[test]
    fn client_shallow_alone_is_not_a_deepen() {
        let mut req = Request::default();
        req.absorb("shallow 0123456789012345678901234567890123456789").unwrap();
        assert!(!req.deepen.requested());
    }
}
