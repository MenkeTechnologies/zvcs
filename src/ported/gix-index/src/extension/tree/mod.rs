use std::cmp::Ordering;

use crate::extension::Signature;

/// The signature for tree extensions
pub const SIGNATURE: Signature = *b"TREE";

///
pub mod verify;

///
pub mod update;

///
pub mod prime;

mod invalidate;

mod decode;
pub use decode::decode;

mod write;

/// `subtree_name_cmp()` (cache-tree.c:49-57): shorter names sort first, and equal-length names
/// compare by `memcmp`.
///
/// This is deliberately *not* plain lexicographic order — `z` sorts before `aa` — and it is the
/// order in which git stores and writes a node's subtree list (`find_subtree()` inserts at the
/// binary-searched position, cache-tree.c:86-104, and `write_one()` dies with
/// "fatal - unsorted cache subtree" on anything else, cache-tree.c:580-584). Keeping the
/// in-memory children in this order is what makes an extension we re-emit byte-identical to the
/// one stock git wrote.
pub(crate) fn subtree_name_cmp(one: &[u8], two: &[u8]) -> Ordering {
    one.len().cmp(&two.len()).then_with(|| one.cmp(two))
}

#[cfg(test)]
mod tests {
    use gix_testtools::size_ok;

    #[test]
    fn size_of_tree() {
        let actual = std::mem::size_of::<crate::extension::Tree>();
        let sha1 = 88;
        let sha256_extra = 16;
        let expected = sha1 + sha256_extra;
        assert!(
            size_ok(actual, expected),
            "the size of this structure should not change unexpectedly: {actual} <~ {expected}"
        );
    }

    #[test]
    fn subtree_name_cmp_is_length_first() {
        use std::cmp::Ordering;
        // The whole point: git orders by length before content, so a one-byte name precedes a
        // two-byte one that would sort earlier lexicographically.
        assert_eq!(super::subtree_name_cmp(b"z", b"aa"), Ordering::Less);
        assert_eq!(super::subtree_name_cmp(b"aa", b"ab"), Ordering::Less);
        assert_eq!(super::subtree_name_cmp(b"aa", b"aa"), Ordering::Equal);
    }
}
