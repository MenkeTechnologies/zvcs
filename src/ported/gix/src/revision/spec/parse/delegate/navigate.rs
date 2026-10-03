use gix_error::{ErrorExt, Exn, OptionExt, ResultExt, bail, message};
use gix_hash::ObjectId;
use gix_index::entry::Stage;
use gix_revision::spec::parse::{
    delegate,
    delegate::{PeelTo, Traversal},
};

use crate::revision::spec::parse::delegate::peel;
use crate::{
    Object,
    bstr::{BStr, ByteSlice},
    ext::ObjectIdExt,
    object,
    revision::spec::parse::{Delegate, delegate::Replacements},
};

impl delegate::Navigate for Delegate<'_> {
    fn traverse(&mut self, kind: Traversal) -> Result<(), Exn> {
        self.unset_disambiguate_call();
        self.follow_refs_to_objects_if_needed_delay_errors();

        let mut replacements = Replacements::default();
        let mut errors = Vec::<(ObjectId, Exn)>::new();
        let objs = match self.objs[self.idx].as_mut() {
            Some(objs) => objs,
            None => {
                bail!(message("Tried to navigate the commit-graph without providing an anchor first").raise_erased())
            }
        };
        let repo = self.repo;

        for obj in objs.iter() {
            match kind {
                Traversal::NthParent(num) => {
                    // `get_parent()` resolves its anchor with
                    // `GET_OID_COMMITTISH` and then `lookup_commit_reference()`,
                    // so `<annotated-tag>^<n>` names a parent of the *tagged*
                    // commit — object-name.c:1031-1053. The replacement is keyed
                    // by the anchor as it was before peeling, since that is the
                    // id still recorded in `objs`.
                    match commit_reference(repo, obj) {
                        Ok(commit) => match commit.parent_ids().nth(num.saturating_sub(1)) {
                            Some(id) => replacements.push((*obj, id.detach())),
                            None => errors.push((
                                *obj,
                                message!(
                                    "Commit {oid} has {available} parents and parent number {desired} is out of range",
                                    oid = commit.id().shorten_or_id(),
                                    desired = num,
                                    available = commit.parent_ids().count(),
                                )
                                .raise_erased(),
                            )),
                        },
                        Err(err) => errors.push((*obj, err)),
                    }
                }
                Traversal::NthAncestor(num) => {
                    // `get_nth_ancestor()` peels through the tag chain with
                    // `lookup_commit_reference()` before walking, so `<tag>~<n>`
                    // counts from the tagged commit — object-name.c:1065-1078.
                    let commit = match commit_reference(repo, obj) {
                        Ok(commit) => commit,
                        Err(err) => {
                            errors.push((*obj, err));
                            continue;
                        }
                    };
                    let id = commit.id().detach().attach(repo);
                    match id
                        .ancestors()
                        .first_parent_only()
                        .all()
                        .expect("cannot fail without sorting")
                        .skip(num)
                        .find_map(Result::ok)
                    {
                        Some(commit) => replacements.push((*obj, commit.id)),
                        None => errors.push((
                            *obj,
                                message!("Commit {oid} has {available} ancestors along the first parent and ancestor number {num} is out of range",
                                    oid = id.shorten_or_id(),
                                    available = id
                                        .ancestors()
                                        .first_parent_only()
                                        .all()
                                        .expect("cannot fail without sorting")
                                        .skip(1)
                                        .count()
                                ).raise_erased()
                        )),
                    }
                }
            }
        }

        handle_errors_and_replacements(&mut self.delayed_errors, objs, errors, &mut replacements)
    }

    fn peel_until(&mut self, kind: PeelTo<'_>) -> Result<(), Exn> {
        self.unset_disambiguate_call();
        self.follow_refs_to_objects_if_needed_delay_errors();

        let mut replacements = Replacements::default();
        let mut errors = Vec::<(ObjectId, Exn)>::new();
        let objs = self.objs[self.idx]
            .as_mut()
            .ok_or_raise_erased(|| message!("Couldn't get object at internal index {idx}", idx = self.idx))?;
        let repo = self.repo;

        match kind {
            PeelTo::ValidObject => {
                for obj in objs.iter() {
                    if let Err(err) = repo.find_object(*obj) {
                        errors.push((*obj, err.raise_erased()));
                    }
                }
            }
            PeelTo::ObjectKind(kind) => {
                let peel = |obj| peel(repo, obj, kind);
                for obj in objs.iter() {
                    match peel(obj) {
                        Ok(replace) => replacements.push((*obj, replace)),
                        Err(err) => errors.push((*obj, err)),
                    }
                }
            }
            PeelTo::Path(path) => {
                let lookup_path = |obj: &ObjectId| {
                    let tree_id = peel(repo, obj, gix_object::Kind::Tree)?;
                    if path.is_empty() {
                        return Ok::<_, Exn>((tree_id, gix_object::tree::EntryKind::Tree.into()));
                    }
                    let mut tree = repo.find_object(tree_id).or_erased()?.into_tree();
                    let entry = tree
                        .peel_to_entry_by_path(gix_path::from_bstr(path))
                        .or_erased()?
                        .ok_or_raise_erased(|| {
                            message!(
                                "Could not find path {path:?} in tree {tree} of parent object {object}",
                                path = path,
                                object = obj.attach(repo).shorten_or_id(),
                                tree = tree_id.attach(repo).shorten_or_id(),
                            )
                        })?;
                    Ok((entry.object_id(), entry.mode()))
                };
                for obj in objs.iter() {
                    match lookup_path(obj) {
                        Ok((replace, mode)) => {
                            if !path.is_empty() {
                                // Technically this is letting the last one win, but so be it.
                                self.paths[self.idx] = Some((path.to_owned(), mode));
                            }
                            replacements.push((*obj, replace));
                        }
                        Err(err) => errors.push((*obj, err)),
                    }
                }
            }
            PeelTo::RecursiveTagObject => {
                for oid in objs.iter() {
                    match oid.attach(repo).object().and_then(Object::peel_tags_to_end) {
                        Ok(obj) => replacements.push((*oid, obj.id)),
                        Err(err) => errors.push((*oid, err.raise_erased())),
                    }
                }
            }
        }

        handle_errors_and_replacements(&mut self.delayed_errors, objs, errors, &mut replacements)
    }

    fn find(&mut self, regex: &BStr, negated: bool) -> Result<(), Exn> {
        self.unset_disambiguate_call();
        self.follow_refs_to_objects_if_needed_delay_errors();

        #[cfg(not(feature = "revparse-regex"))]
        let matches = |message: &BStr| -> bool { message.contains_str(regex) ^ negated };
        #[cfg(feature = "revparse-regex")]
        let matches = match regex::bytes::Regex::new(regex.to_str_lossy().as_ref()) {
            Ok(compiled) => {
                let needs_regex = regex::escape(compiled.as_str()) != regex;
                move |message: &BStr| -> bool {
                    if needs_regex {
                        compiled.is_match(message) ^ negated
                    } else {
                        message.contains_str(regex) ^ negated
                    }
                }
            }
            Err(err) => {
                bail!(err.raise_erased());
            }
        };

        match self.objs[self.idx].as_mut() {
            Some(objs) => {
                let repo = self.repo;
                let mut errors = Vec::<(ObjectId, Exn)>::new();
                let mut replacements = Replacements::default();
                for oid in objs.iter() {
                    // `peel_onion()` treats `^{/<text>}` as `expected_type =
                    // OBJ_COMMIT` and runs `repo_peel_to_type()` before seeding
                    // the one-line search — object-name.c:907-1000. Without that
                    // peel an annotated tag has no ancestry to search.
                    let start = match commit_reference(repo, oid) {
                        Ok(commit) => commit.id,
                        Err(err) => {
                            errors.push((*oid, err));
                            continue;
                        }
                    };
                    // `peel_onion()` hands `get_oid_oneline()` a one-entry list
                    // holding the peeled commit (object-name.c:996-997).
                    let (found, count) = oneline_search(repo, &[start], &matches);
                    match found {
                        Some(id) => replacements.push((*oid, id)),
                        None => errors.push((
                            *oid,
                            message!(
                                "None of {commits_searched} commits from {oid} matched {kind} {regex:?}",
                                regex = regex,
                                commits_searched = count,
                                oid = oid.attach(repo).shorten_or_id(),
                                kind = if cfg!(feature = "revparse-regex") {
                                    "regex"
                                } else {
                                    "text"
                                }
                            )
                            .raise_erased(),
                        )),
                    }
                }
                handle_errors_and_replacements(&mut self.delayed_errors, objs, errors, &mut replacements)
            }
            None => {
                // `:/<text>` with no anchor runs `get_oid_oneline()` over the list
                // `handle_one_ref()` built (object-name.c:1763-1771):
                // `refs_for_each_ref()` visits refs in name order and then
                // `refs_head_ref()` adds HEAD, and `commit_list_insert()`
                // *prepends* each, so the list starts with HEAD followed by the
                // refs in reverse name order. `handle_one_ref()` derefs tags and
                // keeps only commits (object-name.c:1164-1181).
                let references = self.repo.references().or_erased()?;
                let references = references.all().or_erased()?;
                let mut seeds: Vec<ObjectId> = references
                    .peeled()
                    .or_raise_erased(|| message("Couldn't configure iterator for peeling"))?
                    .filter_map(Result::ok)
                    .filter_map(|r| r.detach().peeled)
                    .filter_map(|id| commit_reference(self.repo, &id).ok().map(|c| c.id))
                    .collect();
                if let Some(head) = self
                    .repo
                    .head_id()
                    .ok()
                    .and_then(|id| commit_reference(self.repo, &id).ok().map(|c| c.id))
                {
                    seeds.push(head);
                }
                seeds.reverse();
                let (found, count) = oneline_search(self.repo, &seeds, matches);
                match found {
                    Some(id) => {
                        let objs = self.objs[self.idx].get_or_insert_with(Vec::new);
                        if !objs.contains(&id) {
                            objs.push(id);
                        }
                        Ok(())
                    }
                    None => Err(message!(
                        "None of {commits_searched} commits reached from all references matched {kind} {regex:?}",
                        regex = regex,
                        commits_searched = count,
                        kind = if cfg!(feature = "revparse-regex") {
                            "regex"
                        } else {
                            "text"
                        }
                    )
                    .raise_erased()),
                }
            }
        }
    }

    fn index_lookup(&mut self, path: &BStr, stage: u8) -> Result<(), Exn> {
        let stage = match stage {
            0 => Stage::Unconflicted,
            1 => Stage::Base,
            2 => Stage::Ours,
            3 => Stage::Theirs,
            _ => unreachable!(
                "BUG: driver will not pass invalid stages (and it uses integer to avoid gix-index as dependency)"
            ),
        };
        self.unset_disambiguate_call();
        let index = self.repo.index().or_erased()?;
        match index.entry_by_path_and_stage(path, stage) {
            Some(entry) => {
                let objs = self.objs[self.idx].get_or_insert_with(Vec::new);
                if !objs.contains(&entry.id) {
                    objs.push(entry.id);
                }

                self.paths[self.idx] = Some((
                    path.to_owned(),
                    entry
                        .mode
                        .to_tree_entry_mode()
                        .unwrap_or(gix_object::tree::EntryKind::Blob.into()),
                ));
                Ok(())
            }
            None => {
                let stage_hint = [Stage::Unconflicted, Stage::Base, Stage::Ours, Stage::Theirs]
                    .iter()
                    .filter(|our_stage| **our_stage != stage)
                    .find_map(|stage| index.entry_index_by_path_and_stage(path, *stage).map(|_| *stage));
                let exists = self
                    .repo
                    .workdir()
                    .is_some_and(|root| root.join(gix_path::from_bstr(path)).exists());
                Err(message!(
                    "Path {path:?} did not exist in index at stage {desired_stage}{stage_hint}{exists}",
                    exists = if exists {
                        ". It exists on disk"
                    } else {
                        ". It does not exist on disk"
                    },
                    stage_hint = stage_hint
                        .map(|actual| format!(". It does exist at stage {}", actual as u8))
                        .unwrap_or_default(),
                    desired_stage = stage as u8,
                )
                .raise_erased())
            }
        }
    }
}

/// Port of `lookup_commit_reference()` (commit.c): dereference the whole tag
/// chain hanging off `obj`, then require the result to be a commit. This is what
/// every committish navigation in `object-name.c` runs before it looks at
/// parents, which is why `<annotated-tag>^`, `<annotated-tag>~<n>` and
/// `<annotated-tag>^{/<text>}` all work in git.
fn commit_reference<'repo>(repo: &'repo crate::Repository, obj: &gix_hash::oid) -> Result<crate::Commit<'repo>, Exn> {
    repo.find_object(obj)
        .or_erased()
        .and_then(|obj| obj.peel_tags_to_end().map_err(|err| err.raise_erased()))
        .and_then(|obj| {
            obj.try_into_commit().map_err(|err| {
                let object::try_into::Error { actual, expected, id } = err;
                message!(
                    "Object {oid} was a {actual}, but needed it to be a {expected}",
                    oid = id.attach(repo).shorten_or_id(),
                )
                .raise_erased()
            })
        })
}

fn handle_errors_and_replacements(
    delayed_errors: &mut Vec<Exn>,
    objs: &mut Vec<ObjectId>,
    errors: Vec<(ObjectId, Exn)>,
    replacements: &mut Replacements,
) -> Result<(), Exn> {
    if errors.len() == objs.len() {
        delayed_errors.extend(errors.into_iter().map(|(_, err)| err));
        Err(delayed_errors
            .pop()
            .unwrap_or_else(|| message("BUG: Somehow there was no error but one was expected").raise_erased()))
    } else {
        for (obj, err) in errors {
            if let Some(pos) = objs.iter().position(|o| o == &obj) {
                objs.remove(pos);
            }
            delayed_errors.push(err);
        }
        for (find, replace) in replacements {
            if let Some(pos) = objs.iter().position(|o| o == find) {
                objs.remove(pos);
            }
            if !objs.contains(replace) {
                objs.push(*replace);
            }
        }
        Ok(())
    }
}

/// Port of the walk in `get_oid_oneline()` (object-name.c:1183-1235): the seeds
/// go into a `prio_queue` ordered by `compare_commits_by_commit_date()`
/// (commit.c:923-933, newest first), ties broken by insertion order
/// (prio-queue.c:4-12), and `pop_most_recent_commit()` (commit.c:782-797) queues
/// every not-yet-seen parent of the popped commit. The first commit whose message
/// matches wins. Returns the match and how many commits were looked at.
///
/// A seed listed twice is queued once: git queues both copies, but the later
/// copy pops after the first with the same message and no unseen parents, so it
/// can neither match first nor change the order.
fn oneline_search(
    repo: &crate::Repository,
    seeds: &[ObjectId],
    matches: impl Fn(&BStr) -> bool,
) -> (Option<ObjectId>, usize) {
    use std::{cmp::Reverse, collections::BinaryHeap};

    let commit_date = |id: ObjectId| {
        repo.find_object(id)
            .ok()
            .and_then(|obj| obj.try_into_commit().ok())
            .and_then(|commit| commit.committer().ok().map(|sig| sig.seconds()))
    };
    let mut seen = std::collections::HashSet::new();
    let mut queue = BinaryHeap::new();
    let mut insertion_ctr = 0usize;
    let mut put = |queue: &mut BinaryHeap<_>, id: ObjectId, date: gix_date::SecondsSinceUnixEpoch| {
        queue.push((date, Reverse(insertion_ctr), id));
        insertion_ctr += 1;
    };
    for &id in seeds {
        if seen.insert(id) {
            put(&mut queue, id, commit_date(id).unwrap_or_default());
        }
    }

    let mut count = 0;
    while let Some((_, _, id)) = queue.pop() {
        count += 1;
        let Some(commit) = repo.find_object(id).ok().and_then(|obj| obj.try_into_commit().ok()) else {
            continue;
        };
        // `pop_most_recent_commit()` queues the parents before the caller
        // reads the message; a parent that does not parse is skipped.
        for parent in commit.parent_ids().map(|id| id.detach()) {
            if seen.contains(&parent) {
                continue;
            }
            if let Some(date) = commit_date(parent) {
                seen.insert(parent);
                put(&mut queue, parent, date);
            }
        }
        if matches(commit.message_raw_sloppy()) {
            return (Some(commit.id), count);
        }
    }
    (None, count)
}
