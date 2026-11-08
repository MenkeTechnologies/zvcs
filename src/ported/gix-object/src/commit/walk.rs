//! What a history walk reads of a commit: the tree, the parents and the
//! committer date, and nothing else.
//!
//! git's walks parse a commit with `parse_commit_buffer()` (commit.c:516-598),
//! which checks the `tree` and `parent` lines strictly and reads the date with
//! `parse_commit_date()` (commit.c:128-194), a scan that never fails: an author
//! or committer line it cannot read yields date 0. The idents are left to fsck
//! and to the pretty printers, so a commit with a malformed author, a missing
//! committer or a bad date is still walked, where [`CommitRefIter`](crate::CommitRefIter)
//! stops at the first signature it cannot decode.

use gix_hash::ObjectId;
use smallvec::SmallVec;

/// The parts of a commit that `parse_commit_buffer()` keeps
/// (commit.c:516-598).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkInfo {
    /// The commit's tree.
    pub tree: ObjectId,
    /// The parents, in the order the `parent` lines name them.
    pub parents: SmallVec<[ObjectId; 2]>,
    /// The committer date as `parse_commit_date()` reads it: 0 where the
    /// author or committer line cannot be read.
    pub date: gix_date::SecondsSinceUnixEpoch,
}

impl WalkInfo {
    /// Parse `data`, a commit object's body, as `parse_commit_buffer()` does.
    ///
    /// Fails only where git fails: a first line that is not `tree <hex>\n`
    /// ("bogus commit object", "bad tree pointer") or a `parent` line that is
    /// not `parent <hex>\n` ("bad parents in commit").
    pub fn from_bytes(data: &[u8], hash_kind: gix_hash::Kind) -> Result<Self, crate::decode::Error> {
        let hexsz = hash_kind.len_in_hex();
        // `tree_entry_len = hexsz + 5`, `parent_entry_len = hexsz + 7`
        // (commit.c:523-524).
        let tree_entry_len = hexsz + 5;
        let parent_entry_len = hexsz + 7;
        let tail = data.len();

        // commit.c:539-545
        if tail <= tree_entry_len + 1 || !data.starts_with(b"tree ") || data[tree_entry_len] != b'\n' {
            return Err(crate::decode::Error);
        }
        let tree = hex_id(&data[5..tree_entry_len])?;
        let mut pos = tree_entry_len + 1;

        // commit.c:556-575: the loop runs while a whole `parent` line could
        // still fit; a line it enters must then be well formed.
        let mut parents = SmallVec::new();
        while pos + parent_entry_len < tail && data[pos..].starts_with(b"parent ") {
            if tail <= pos + parent_entry_len + 1 || data[pos + parent_entry_len] != b'\n' {
                return Err(crate::decode::Error);
            }
            parents.push(hex_id(&data[pos + 7..pos + parent_entry_len])?);
            pos += parent_entry_len + 1;
        }

        Ok(WalkInfo {
            tree,
            parents,
            date: parse_commit_date(&data[pos..]),
        })
    }
}

impl crate::CommitRefIter<'_> {
    /// Read the commit this iterator was created on as [`WalkInfo::from_bytes()`]
    /// does. Only meaningful before the iterator is advanced.
    pub fn walk_info(&self) -> Result<WalkInfo, crate::decode::Error> {
        debug_assert!(
            matches!(self.state, super::ref_iter::State::Tree),
            "walk_info() reads the whole commit and needs an iterator that was not advanced"
        );
        WalkInfo::from_bytes(self.data, self.hash_kind)
    }
}

/// `get_oid_hex()`: exactly `hex` hex digits, either case.
fn hex_id(hex: &[u8]) -> Result<ObjectId, crate::decode::Error> {
    ObjectId::from_hex(hex).map_err(|_| crate::decode::Error)
}

/// Port of `parse_commit_date()` (commit.c:128-194): `buf` starts right after
/// the last `parent` line. The committer line is read backwards from its end
/// to the last `>`, so garbage in the name or email does not matter; anything
/// unreadable is date 0.
pub fn parse_commit_date(buf: &[u8]) -> gix_date::SecondsSinceUnixEpoch {
    let tail = buf.len();
    // commit.c:133-136: `buf + 6 >= tail` and a 6-byte compare, so
    // `authorX` passes.
    if 6 >= tail || &buf[..6] != b"author" {
        return 0;
    }
    // commit.c:137-138: step past the author line's newline.
    let committer = match buf.iter().position(|&b| b == b'\n') {
        Some(nl) => &buf[nl + 1..],
        None => return 0,
    };
    // commit.c:139-142
    if 9 >= committer.len() || &committer[..9] != b"committer" {
        return 0;
    }
    // commit.c:150-156: from the end of the line back to just past the last
    // '>'.
    let Some(eol) = committer.iter().position(|&b| b == b'\n') else {
        return 0;
    };
    let Some(gt) = committer[..eol].iter().rposition(|&b| b == b'>') else {
        return 0;
    };
    let mut date = &committer[gt + 1..eol];
    // commit.c:175-178: git's isspace() is ' ', '\t', '\n' and '\r'
    // (git-compat-util.h GIT_SPACE); the newline is out of reach here.
    while let Some((&b, rest)) = date.split_first() {
        if !matches!(b, b' ' | b'\t' | b'\r') {
            break;
        }
        date = rest;
    }
    match date.first() {
        Some(b) if b.is_ascii_digit() || *b == b'-' => {}
        _ => return 0,
    }
    parse_timestamp(date)
}

/// `parse_timestamp()` is `strtoumax(.., 10)` (git-compat-util.h): it reads
/// as many digits as there are, saturates at `UINTMAX_MAX` on overflow and
/// negates the unsigned result after a '-'. `timestamp_t` is unsigned, so the
/// bits are kept and handed back as git's walks would compare them only after
/// the cast.
fn parse_timestamp(s: &[u8]) -> gix_date::SecondsSinceUnixEpoch {
    let (negative, digits) = match s.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, s),
    };
    let mut value: u64 = 0;
    for &b in digits.iter().take_while(|b| b.is_ascii_digit()) {
        match value.checked_mul(10).and_then(|v| v.checked_add(u64::from(b - b'0'))) {
            Some(v) => value = v,
            None => {
                // ERANGE: strtoumax() returns UINTMAX_MAX whatever the sign.
                value = u64::MAX;
                return value as gix_date::SecondsSinceUnixEpoch;
            }
        }
    }
    if negative {
        value = value.wrapping_neg();
    }
    value as gix_date::SecondsSinceUnixEpoch
}

#[cfg(test)]
mod tests {
    use super::*;

    const TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    fn info(rest: &str) -> Result<WalkInfo, crate::decode::Error> {
        WalkInfo::from_bytes(format!("tree {TREE}\n{rest}").as_bytes(), gix_hash::Kind::Sha1)
    }

    #[test]
    fn date_survives_a_malformed_author() {
        let i = info(&format!(
            "parent {TREE}\nauthor bad <a@e 1700000000 +0000\ncommitter C <c@e> 1700000001 +0000\n\nm\n"
        ))
        .unwrap();
        assert_eq!(i.parents.len(), 1);
        assert_eq!(i.date, 1700000001);
    }

    #[test]
    fn unreadable_idents_are_date_zero() {
        assert_eq!(info("committer C <c@e> 5 +0000\n\nm\n").unwrap().date, 0);
        assert_eq!(info("author A <a@e> 5 +0000\n\nm\n").unwrap().date, 0);
        assert_eq!(info("author A <a@e> 5 +0000\ncommitter C <c@e> abc +0000\n\nm\n").unwrap().date, 0);
        assert_eq!(info("author A <a@e> 5 +0000\ncommitter C c@e 7 +0000\n\nm\n").unwrap().date, 0);
    }

    #[test]
    fn date_is_read_as_far_as_it_goes() {
        assert_eq!(info("author A <a@e> 5 +0000\ncommitter C <c@e> 7 +00x0\n\nm\n").unwrap().date, 7);
        assert_eq!(info("author A <a@e> 5 +0000\ncommitter C <c@e>  123foo\n\nm\n").unwrap().date, 123);
    }

    #[test]
    fn bad_tree_or_parent_fails() {
        assert!(WalkInfo::from_bytes(b"tree zz\n", gix_hash::Kind::Sha1).is_err());
        assert!(info("parent 1234\nauthor A <a@e> 5 +0000\n").is_ok_and(|i| i.parents.is_empty()));
        assert!(info(&format!("parent {}\n", "z".repeat(40))).is_err());
    }
}
