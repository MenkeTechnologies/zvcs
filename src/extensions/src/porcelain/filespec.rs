//! git's `diff_filespec` helpers, shared by every port that renders a diff.
//!
//! `diff_populate_filespec()` (diff.c) is the one place git decides what bytes a
//! tree entry *diffs as*, and it is not always "the blob". A gitlink names a
//! commit in another repository — an object this object database has no reason to
//! contain — so git substitutes the single line `Subproject commit <oid>` and
//! diffs that. Everything downstream (the `--stat` line counts, the binary
//! heuristic, the patch text) is computed over whatever that function produced,
//! which is why these three helpers belong together and are stated once.

use anyhow::Result;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};
use gix::hash::ObjectId;

/// The bytes to diff for an entry: a real blob is read from the object database; a
/// submodule (commit entry) is rendered as its `Subproject commit <oid>` line.
pub(crate) fn content_of(repo: &gix::Repository, id: ObjectId, is_submodule: bool) -> Result<Vec<u8>> {
    if is_submodule {
        Ok(format!("Subproject commit {}\n", id.to_hex()).into_bytes())
    } else {
        Ok(repo.find_object(id)?.detach().data)
    }
}

/// git's binary heuristic: a NUL byte within the first 8000 bytes.
pub(crate) fn is_binary(data: &[u8]) -> bool {
    data.iter().take(8000).any(|&b| b == 0)
}

/// Total added and removed lines, for `--stat`. Uses the same hunk machinery as
/// the patch so the two can never disagree about what changed.
pub(crate) fn count_changed_lines(old: &[u8], new: &[u8]) -> Result<(usize, usize)> {
    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    let counter = LineCounter {
        added: 0,
        deleted: 0,
    };
    Ok(UnifiedDiff::new(&diff, &input, counter, ContextSize::symmetrical(3)).consume()?)
}

/// Counts changed lines, ignoring context.
struct LineCounter {
    added: usize,
    deleted: usize,
}

impl ConsumeHunk for LineCounter {
    type Out = (usize, usize);

    fn consume_hunk(&mut self, _header: HunkHeader, lines: &[(DiffLineKind, &[u8])]) -> std::io::Result<()> {
        for &(kind, _) in lines {
            match kind {
                DiffLineKind::Add => self.added += 1,
                DiffLineKind::Remove => self.deleted += 1,
                DiffLineKind::Context => {}
            }
        }
        Ok(())
    }

    fn finish(self) -> (usize, usize) {
        (self.added, self.deleted)
    }
}
