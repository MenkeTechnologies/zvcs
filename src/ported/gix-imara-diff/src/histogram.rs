// Modified for gitoxide from the upstream imara-diff crate.
// Upstream source: git cat-file -p 32d1e45d3df061e6ccba6db7fdce92db29e345d8:src/histogram.rs

use crate::histogram::lcs::find_lcs;
use crate::histogram::list_pool::{ListHandle, ListPool};
use crate::intern::Token;
use crate::{Algorithm, Diff};

mod lcs;
mod list_pool;

/// `index.max_chain_length`, `xdiff/xhistogram.c:284` (git 2.55.0): the largest number of
/// occurrences a line may have and still be allowed to anchor an LCS.
///
/// ```text
/// index.max_chain_length = 64;
/// ...
/// index.cnt = index.max_chain_length + 1;
/// ...
/// if (index.has_common && index.max_chain_length < index.cnt)
///         ret = 1;
/// ```
///
/// `index.cnt` starts one above the limit and is lowered to the occurrence count of every
/// anchor that is accepted, so the search succeeds exactly when some line occurring at most
/// `64` times could anchor it; otherwise `xdl_fall_back_diff()` hands the region to Myers.
const MAX_CHAIN_LEN: u32 = 64;

/// `fall_back_to_classic_diff()`: a region no line is rare enough to anchor is Myers' problem.
///
/// Upstream clears the algorithm bits and calls `xdl_fall_back_diff()`
/// (`xdiff/xhistogram.c:229-239`), which copies the region into fresh `mmfile_t`s and
/// re-enters `xdl_do_diff()` (`xdiff/xutils.c:453-482`). Re-entering matters and is why this
/// cannot call `myers::diff` directly: `xdl_do_diff()` runs `xdl_prepare_env()` first, and
/// with the algorithm bits cleared that no longer skips `xdl_optimize_ctxs()`, so the region
/// gets `xdl_trim_ends()` and `xdl_cleanup_records()` applied to it — which is exactly what
/// [`Diff::compute_with`] does for [`Algorithm::Myers`] and what a bare `myers::diff` skips.
/// The region *is* the whole file to that sub-diff, so it is also its own untrimmed sequence.
///
/// Same shape as `patience::fall_back` and `compact::fall_back_diff`, which port the two
/// other call sites of `xdl_fall_back_diff()`.
fn fall_back(before: &[Token], after: &[Token], removed: &mut [bool], added: &mut [bool]) {
    // Only the histogram algorithm reads this, and Myers is what runs here.
    let num_tokens = before
        .iter()
        .chain(after)
        .map(|t| u32::from(*t) + 1)
        .max()
        .unwrap_or(0);
    let mut sub = Diff::default();
    sub.compute_with(Algorithm::Myers, before, after, num_tokens);
    for (slot, flag) in removed.iter_mut().enumerate() {
        *flag = sub.is_removed(slot as u32);
    }
    for (slot, flag) in added.iter_mut().enumerate() {
        *flag = sub.is_added(slot as u32);
    }
}

/// State for computing histogram-based diffs.
struct Histogram {
    /// Tracks where each token appears in the "before" sequence.
    token_occurrences: Vec<ListHandle>,
    /// Memory pool for efficiently storing occurrence lists.
    pool: ListPool,
}

/// Computes a diff using the histogram algorithm.
///
/// # Parameters
///
/// * `before` - The token sequence from the first file, before changes.
/// * `after` - The token sequence from the second file, after changes.
/// * `removed` - Output array marking removed tokens
/// * `added` - Output array marking added tokens
/// * `num_tokens` - The total number of distinct tokens
pub fn diff(before: &[Token], after: &[Token], removed: &mut [bool], added: &mut [bool], num_tokens: u32) {
    let mut histogram = Histogram::new(num_tokens);
    histogram.run(before, after, removed, added);
}

impl Histogram {
    fn new(num_buckets: u32) -> Histogram {
        Histogram {
            token_occurrences: vec![ListHandle::default(); num_buckets as usize],
            pool: ListPool::new(2 * num_buckets),
        }
    }

    fn clear(&mut self) {
        self.pool.clear();
    }

    fn token_occurrences(&self, token: Token) -> &[u32] {
        self.token_occurrences[token.0 as usize].as_slice(&self.pool)
    }

    /// `rec->cnt`: how often the token occurs in the *before* file, counted exactly.
    ///
    /// Not the length of the stored occurrence list, which stops growing past
    /// [`MAX_CHAIN_LEN`] + 1 entries. git caps `rec->cnt` at `MAX_CNT`
    /// (`xdiff/xhistogram.c:47`), `UINT_MAX`, so within any reachable input it is the true
    /// count, and `try_lcs()` compares it against `index->cnt` to decide whether a line is
    /// too common to anchor on. Reporting a saturated length instead would let a line that
    /// occurs a thousand times pass a limit git applies at 64.
    fn num_token_occurrences(&self, token: Token) -> u32 {
        self.token_occurrences[token.0 as usize].count(&self.pool)
    }

    fn populate(&mut self, file: &[Token]) {
        for (i, &token) in file.iter().enumerate() {
            self.token_occurrences[token.0 as usize].push(i as u32, &mut self.pool);
        }
    }

    fn run(&mut self, mut before: &[Token], mut after: &[Token], mut removed: &mut [bool], mut added: &mut [bool]) {
        loop {
            if before.is_empty() {
                added.fill(true);
                return;
            } else if after.is_empty() {
                removed.fill(true);
                return;
            }

            self.populate(before);
            match find_lcs(before, after, self) {
                // no lcs was found, that means that file1 and file2 two have nothing in common
                Some(lcs) if lcs.len == 0 => {
                    added.fill(true);
                    removed.fill(true);
                    return;
                }
                Some(lcs) => {
                    self.run(
                        &before[..lcs.before_start as usize],
                        &after[..lcs.after_start as usize],
                        &mut removed[..lcs.before_start as usize],
                        &mut added[..lcs.after_start as usize],
                    );

                    // this is equivalent to (tail) recursion but implement as a loop for efficiency reasons
                    let before_end = lcs.before_start + lcs.len;
                    before = &before[before_end as usize..];
                    removed = &mut removed[before_end as usize..];

                    let after_end = lcs.after_start + lcs.len;
                    after = &after[after_end as usize..];
                    added = &mut added[after_end as usize..];
                }
                None => {
                    // Every line left in this region occurs too often to anchor on, so
                    // `find_lcs()` reported `lcs_found` and git hands the region to Myers
                    // (`fall_back_to_classic_diff()`, `xdiff/xhistogram.c:229-239`).
                    fall_back(before, after, removed, added);
                    return;
                }
            }
        }
    }
}
