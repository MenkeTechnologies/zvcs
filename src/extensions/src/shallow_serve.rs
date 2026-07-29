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

/// `get_shallow_commits()` (shallow.c:59-108): breadth-first from `tips`, cutting
/// at `depth` hops. The tips are depth 1, so `depth == 1` makes every tip a
/// boundary and fetches nothing behind them.
///
/// Returns the visited commits and, separately, the ones that are boundaries.
fn walk_by_depth(
    repo: &gix::Repository,
    tips: &[ObjectId],
    depth: u32,
    grafts: &HashSet<ObjectId>,
) -> (Vec<ObjectId>, HashSet<ObjectId>) {
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
    (visited, boundary)
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
        if request.deepen.relative {
            // `deepen_relative`: the depth is measured from the client's own
            // boundary, so the walk starts there — one hop deeper, because the
            // client's boundary commit is a commit it already has.
            let reachable = reachable_client_shallows(repo, &tips, &grafts, &request.client_shallow);
            if reachable.is_empty() {
                walk_by_depth(repo, &tips, depth, &grafts)
            } else {
                let (mut visited, boundary) =
                    walk_by_depth(repo, &reachable, depth.saturating_add(1), &grafts);
                // Everything between the wants and the client's boundary is
                // already the client's, but it still belongs to the window — and
                // the walk for it stops *at* that boundary, since anything behind
                // it is what the relative walk above is deciding about.
                let mut stop = grafts.clone();
                stop.extend(request.client_shallow.iter().copied());
                let (near, _) = walk_by_depth(repo, &tips, u32::MAX, &stop);
                let inside: HashSet<ObjectId> = visited.iter().copied().collect();
                visited.extend(near.into_iter().filter(|id| !inside.contains(id)));
                (visited, boundary)
            }
        } else {
            walk_by_depth(repo, &tips, depth, &grafts)
        }
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

    Boundary { shallow, unshallow, commits: visited }
}

/// `get_reachable_list()` (upload-pack.c:620-664): the client's shallow commits
/// that this fetch's wants can actually reach, which is where a relative deepen
/// measures from.
fn reachable_client_shallows(
    repo: &gix::Repository,
    tips: &[ObjectId],
    grafts: &HashSet<ObjectId>,
    client_shallow: &[ObjectId],
) -> Vec<ObjectId> {
    if client_shallow.is_empty() {
        return Vec::new();
    }
    let (visited, _) = walk_by_depth(repo, tips, u32::MAX, grafts);
    let reachable: HashSet<ObjectId> = visited.into_iter().collect();
    client_shallow.iter().copied().filter(|id| reachable.contains(id)).collect()
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
    walk_by_depth(repo, &tips, u32::MAX, grafts).0.into_iter().collect()
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
    walk_by_depth(repo, &tips, u32::MAX, &grafts).0
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
    let want_closure = crate::porcelain::push_proto::expand_roots(repo, &roots);

    let client = client_side_commits(repo, haves, client_shallow);
    if client.is_empty() {
        return want_closure.into_iter().collect();
    }
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
