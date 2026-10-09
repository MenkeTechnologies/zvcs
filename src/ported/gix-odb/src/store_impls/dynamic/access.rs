use crate::Store;

impl Store {
    /// The root path at which we expect to find all objects and packs, and which is the source of the
    /// alternate file traversal in case there are linked repositories.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The kind of object hash to assume when dealing with pack indices and pack data files.
    pub fn object_hash(&self) -> gix_hash::Kind {
        self.object_hash
    }

    /// Whether or not we are allowed to use multi-pack indices
    pub fn use_multi_pack_index(&self) -> bool {
        self.use_multi_pack_index
    }

    /// An iterator over replacements from object-ids `X` to `X-replaced` as `(X, X-replaced)`, sorted by the original id `X`.
    pub fn replacements(&self) -> impl Iterator<Item = (gix_hash::ObjectId, gix_hash::ObjectId)> + '_ {
        self.replacements.iter().copied()
    }
}

/// Port of `do_lookup_replace_object()` (replace-object.c): follow `refs/replace/` links from `id`
/// for at most `MAXREPLACEDEPTH` (5) steps and return the object finally read.
///
/// A chain that is still replacing after five steps - which includes an object replaced by itself -
/// is `die("replace depth too high for object %s")`: `fatal:` on stderr and exit 128, from whichever
/// code asked for the object, so nothing the caller would have done next happens.
pub(crate) fn follow_replacements<'a>(
    replacements: &'a [(gix_hash::ObjectId, gix_hash::ObjectId)],
    id: &'a gix_hash::oid,
) -> &'a gix_hash::oid {
    const MAX_REPLACE_DEPTH: usize = 5;
    let mut current = id;
    for _ in 0..MAX_REPLACE_DEPTH {
        match replacements.binary_search_by(|(replaced, _)| replaced.as_ref().cmp(current)) {
            Ok(pos) => current = replacements[pos].1.as_ref(),
            Err(_) => return current,
        }
    }
    eprintln!("fatal: replace depth too high for object {id}");
    std::process::exit(128);
}
