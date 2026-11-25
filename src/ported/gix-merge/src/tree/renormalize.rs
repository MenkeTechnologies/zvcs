//! merge-ort's attribute source under `merge.renormalize` (merge-ort.c, git v2.56.0).
//!
//! `renormalize_buffer()` needs attributes, which git can only read from a worktree or an
//! `index_state`, and merge-ort has no index. So `initialize_attr_index()`
//! (merge-ort.c:2049-2108) builds a fake one holding nothing but the root `.gitattributes`, the
//! first time a content merge or `blob_unchanged()` asks for attributes, and only when
//! renormalizing (merge-ort.c:2064-2065).

use bstr::BString;
use gix_hash::ObjectId;
use gix_object::{FindExt, tree::EntryMode};

use crate::tree::Error;

const GITATTRIBUTES_FILE: &str = ".gitattributes";

/// Install the attribute stack the fake index of `initialize_attr_index()` describes as
/// `blob_merge.attr_stack`, for the merge of `trees` (`[base, side1, side2]`).
///
/// It does nothing unless `blob_merge` renormalizes, as merge-ort leaves the fake index empty then.
pub(super) fn install_attr_index(
    blob_merge: &mut crate::blob::Platform,
    objects: &impl gix_object::FindObjectOrHeader,
    trees: [&gix_hash::oid; 3],
) -> Result<(), Error> {
    if blob_merge.filter_mode != crate::blob::pipeline::Mode::Renormalize {
        return Ok(());
    }
    let Some(template) = blob_merge.renormalize_attr_stack.as_ref() else {
        return Ok(());
    };
    let mut stack = template.clone();
    let mappings = attr_index_blob(objects, trees)?
        .map(|id| (BString::from(GITATTRIBUTES_FILE), id))
        .into_iter()
        .collect();
    stack.set_id_mappings(mappings);
    blob_merge.attr_stack = stack;
    Ok(())
}

/// The blob `read_blob_data_from_index()` (read-cache.c:3477-3511) finds for `.gitattributes` in the
/// index `initialize_attr_index()` builds from the path's `merged_info`.
///
/// That index is built while the path is still as `collect_merge_info_callback()`
/// (merge-ort.c:1259-1376) left it, since `process_entries()` reaches `.gitattributes` after every
/// path sorting above it. A path resolved there is one stage-0 entry holding its result. An
/// unresolved one gets an entry per stage it has a file in, at stage *numbers* 0, 1 and 2 for the
/// base and the two sides, added in that order, and each `add_index_entry()` is followed by a
/// `get_stream_filter()` whose attribute check builds the root's attribute stack, which attr.c keeps
/// for the rest of the merge (merge-ort.c:2070-2104). So the first entry added decides: the lookup
/// takes a stage-0 entry, or else a stage-2 one (read-cache.c:3486-3499), which makes it the base's
/// file when there is one, nothing when the first side has one, and the second side's otherwise.
/// Nothing found reads the worktree's file instead. A tree or a submodule is not read as
/// attributes (read-cache.c:3502-3507).
fn attr_index_blob(
    objects: &impl gix_object::FindObjectOrHeader,
    trees: [&gix_hash::oid; 3],
) -> Result<Option<ObjectId>, Error> {
    let mut names: [Option<(EntryMode, ObjectId)>; 3] = [None; 3];
    let mut buf = Vec::new();
    for (name, tree) in names.iter_mut().zip(trees) {
        let tree = objects.find_tree(tree, &mut buf)?;
        *name = tree
            .entries
            .iter()
            .find(|entry| entry.filename == GITATTRIBUTES_FILE)
            .map(|entry| (entry.mode, entry.oid.to_owned()));
    }
    let is_file = |name: &Option<(EntryMode, ObjectId)>| name.is_some_and(|(mode, _)| !mode.is_tree());
    let same = |a: &Option<(EntryMode, ObjectId)>, b: &Option<(EntryMode, ObjectId)>| {
        matches!((a, b), (Some(a), Some(b)) if a.0.kind() == b.0.kind() && a.1 == b.1)
    };
    let side1_matches_mbase = same(&names[0], &names[1]);
    let side2_matches_mbase = same(&names[0], &names[2]);
    let sides_match = same(&names[1], &names[2]);
    let filemask = names
        .iter()
        .enumerate()
        .fold(0u8, |mask, (i, name)| if is_file(name) { mask | (1 << i) } else { mask });

    let resolved = if side1_matches_mbase && side2_matches_mbase {
        Some(0)
    } else if filemask == 0x07 && sides_match {
        Some(1)
    } else if filemask == 0x07 && side1_matches_mbase {
        Some(2)
    } else if filemask == 0x07 && side2_matches_mbase {
        Some(1)
    } else {
        None
    };
    let found = match resolved {
        Some(stage) => names[stage],
        None if filemask & 1 != 0 => names[0],
        None if filemask & 2 != 0 => None,
        None => names[2],
    };
    Ok(found.filter(|(mode, _)| mode.is_blob_or_symlink()).map(|(_, id)| id))
}
