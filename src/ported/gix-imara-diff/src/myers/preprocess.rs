// Modified for gitoxide from the upstream imara-diff crate.
// Upstream source: git cat-file -p 32d1e45d3df061e6ccba6db7fdce92db29e345d8:src/myers/preprocess.rs
//
// The upstream file re-implemented git's record-discarding pass from memory. It has since
// been replaced by a line-for-line port of `xdl_trim_ends()`, `xdl_cleanup_records()` and
// `xdl_clean_mmatch()` from git 2.55.0's `xdiff/xprepare.c`, because a different set of
// discarded records makes the Myers edit script diverge from `git diff --diff-algorithm=myers`.

use crate::intern::Token;
use crate::myers::sqrt;

/// `XDL_KPDIS_RUN` from `xdiff/xprepare.c`.
const KPDIS_RUN: i64 = 4;
/// `XDL_MAX_EQLIMIT` from `xdiff/xprepare.c`.
const MAX_EQLIMIT: u64 = 1024;
/// `XDL_SIMSCAN_WINDOW` from `xdiff/xprepare.c`.
const SIMSCAN_WINDOW: isize = 100;

/// The three states `xdl_cleanup_records()` puts into its `action1`/`action2` arrays.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    /// `DISCARD` — the record has no counterpart in the other file at all.
    Discard,
    /// `KEEP` — the record has few enough counterparts to be worth diffing.
    Keep,
    /// `INVESTIGATE` — the record has so many counterparts that it is only kept when it is
    /// not sitting inside a run of discardable records.
    Investigate,
}

/// Port of the record-discarding half of `xdl_prepare_env()`: `xdl_cleanup_records()`.
///
/// `before` and `after` are already git's `recs[dstart..=dend]` for each file — the equivalent
/// of `xdl_trim_ends()` is [`Diff::compute_with`](crate::Diff::compute_with), which strips the
/// common prefix and suffix before dispatching to an algorithm. Records outside that range are
/// neither kept nor marked: they are the common prefix and suffix and are unchanged by
/// construction.
///
/// `untrimmed_before`/`untrimmed_after` are the same files *before* that strip. Only the
/// per-token match counts and the record counts come from them, because git classifies every
/// record in `xdl_prepare_ctx()` — before trimming — and `xdl_cleanup_records()` then reads
/// those whole-file counts. Counting on the trimmed range instead makes a record whose only
/// counterparts sit in the stripped prefix or suffix look unmatched, and it gets discarded.
///
/// Records that survive are returned in [`PreprocessedFile`] (git's `reference_index`/`nreff`);
/// records that are discarded are marked in `removed`/`added` (git's `changed`) and never reach
/// the Myers search.
pub fn preprocess<'a>(
    before: &[Token],
    after: &[Token],
    removed: &'a mut [bool],
    added: &'a mut [bool],
    need_min: bool,
    untrimmed_before: &[Token],
    untrimmed_after: &[Token],
) -> (PreprocessedFile, PreprocessedFile) {
    let (occurrences_before, occurrences_after) = class_counts(untrimmed_before, untrimmed_after);
    // git indexes `changed` by the untrimmed record number while `removed`/`added` here are
    // already offset to `dstart`, so the offset within these slices is simply 0.
    let dstart = 0;

    let mlim1 = eqlimit(untrimmed_before.len(), need_min);
    let mlim2 = eqlimit(untrimmed_after.len(), need_min);
    let action1 = classify(before, dstart, before.len() as isize - 1, &occurrences_after, mlim1);
    let action2 = classify(after, dstart, after.len() as isize - 1, &occurrences_before, mlim2);

    let file1 = resolve(&action1, before, dstart, removed);
    let file2 = resolve(&action2, after, dstart, added);
    (file1, file2)
}

/// Counts, per interned token, how often it occurs in each file. Equivalent to the
/// `rcrec->len1`/`rcrec->len2` counters `xdl_classify_record()` maintains, with the interned
/// [`Token`] standing in for git's `minimal_perfect_hash`.
fn class_counts(file1: &[Token], file2: &[Token]) -> (Vec<u32>, Vec<u32>) {
    fn count(file: &[Token]) -> Vec<u32> {
        let mut counts = Vec::new();
        for token in file {
            let bucket = token.0 as usize;
            if bucket >= counts.len() {
                counts.resize(bucket + 1, 0u32);
            }
            counts[bucket] += 1;
        }
        counts
    }
    (count(file1), count(file2))
}

/// `mlim1`/`mlim2` from `xdl_cleanup_records()`. Under `XDF_NEED_MINIMAL` git sets the limit to
/// `PTRDIFF_MAX`, i.e. nothing is ever investigated and only unmatched records are discarded.
fn eqlimit(nrec: usize, need_min: bool) -> u64 {
    if need_min {
        u64::MAX
    } else {
        (sqrt(nrec) as u64).min(MAX_EQLIMIT)
    }
}

/// The `action1[i] = DISCARD/KEEP/INVESTIGATE` initialization loop of `xdl_cleanup_records()`.
fn classify(file: &[Token], off: usize, dend: isize, occurrences_other: &[u32], mlim: u64) -> Vec<Action> {
    let len = (dend - off as isize + 1).max(0) as usize;
    (0..len)
        .map(|i| {
            let bucket = file[i + off].0 as usize;
            let nm = occurrences_other.get(bucket).copied().unwrap_or(0) as u64;
            if nm == 0 {
                Action::Discard
            } else if nm < mlim {
                Action::Keep
            } else {
                Action::Investigate
            }
        })
        .collect()
}

/// The second half of `xdl_cleanup_records()`: resolve every `INVESTIGATE` with
/// [`clean_mmatch`], then either append the record to `reference_index` or set `changed`.
fn resolve(action: &[Action], file: &[Token], off: usize, changed: &mut [bool]) -> PreprocessedFile {
    let mut tokens = Vec::with_capacity(action.len());
    let mut indices = Vec::with_capacity(action.len());
    for (i, &initial) in action.iter().enumerate() {
        let keep = match initial {
            Action::Keep => true,
            Action::Discard => false,
            Action::Investigate => !clean_mmatch(action, i),
        };
        if keep {
            indices.push((i + off) as u32);
            tokens.push(file[i + off]);
        } else {
            changed[i + off] = true;
        }
    }
    PreprocessedFile { indices, tokens }
}

/// Port of `xdl_clean_mmatch()`.
///
/// Scans the runs of non-`KEEP` records on either side of `i` — which is always an
/// `INVESTIGATE` record — and reports whether the multi-match record sits inside a run that is
/// dominated by unmatched records, in which case it is discarded along with them.
///
/// Both run counters start at 1 for the record at `i` itself, so `i` contributes 2 to the
/// combined `rpdis` total; that double count is git's behaviour and is load-bearing for the
/// `rpdis * 4 < rpdis + rdis` verdict on short runs.
fn clean_mmatch(action: &[Action], i: usize) -> bool {
    let len = action.len() as isize;
    let i = i as isize;

    // Limits the window that is examined during the similar-lines scan. The loops below stop
    // when action[i - r] == KEEP, but there are corner cases where the loop proceeds all the
    // way to the extremities, causing huge performance penalties on big files.
    let mut s = 0;
    let mut e = len - 1;
    if i - s > SIMSCAN_WINDOW {
        s = i - SIMSCAN_WINDOW;
    }
    if e - i > SIMSCAN_WINDOW {
        e = i + SIMSCAN_WINDOW;
    }

    let (mut rdis0, mut rpdis0) = (0i64, 1i64);
    let mut r = 1;
    while i - r >= s {
        match action[(i - r) as usize] {
            Action::Discard => rdis0 += 1,
            Action::Investigate => rpdis0 += 1,
            Action::Keep => break,
        }
        r += 1;
    }
    // A run of nothing but multi-match records before `i` means `i` is not sitting in the
    // middle of unmatched lines, so it is kept.
    if rdis0 == 0 {
        return false;
    }

    let (mut rdis1, mut rpdis1) = (0i64, 1i64);
    let mut r = 1;
    while i + r <= e {
        match action[(i + r) as usize] {
            Action::Discard => rdis1 += 1,
            Action::Investigate => rpdis1 += 1,
            Action::Keep => break,
        }
        r += 1;
    }
    if rdis1 == 0 {
        return false;
    }

    rdis1 += rdis0;
    rpdis1 += rpdis0;

    rpdis1 * KPDIS_RUN < rpdis1 + rdis1
}

/// A file after preprocessing has removed unmatched tokens.
#[derive(Debug)]
pub struct PreprocessedFile {
    /// Maps from new token positions to original positions in the unpreprocessed file.
    /// This is git's `xdf->reference_index`, truncated to `xdf->nreff` entries.
    pub indices: Vec<u32>,
    /// The tokens that remain after preprocessing.
    pub tokens: Vec<Token>,
}

#[cfg(test)]
mod tests {
    use super::{Action, clean_mmatch};

    #[test]
    fn common_line_pruning_ignores_distant_context() {
        let mut action = vec![Action::Keep; 700];
        action[100..400].fill(Action::Discard);
        action[400..450].fill(Action::Discard);
        action[450..500].fill(Action::Investigate);
        action[500..550].fill(Action::Investigate);
        action[550..600].fill(Action::Discard);

        assert!(
            !clean_mmatch(&action, 500),
            "only the last 100 items before the current line should contribute to the backward scan"
        );
    }

    #[test]
    fn multi_match_line_counted_on_both_runs() {
        // 4 discarded records on each side of a lone multi-match record. git counts the record
        // at `i` once per run, so rpdis == 2 and rdis == 8: 2 * 4 < 2 + 8 holds and the record
        // is discarded. Counting it a single time (rpdis == 1) would give 1 * 4 < 1 + 8, which
        // also holds, so pick a run length where the two disagree instead.
        let mut action = vec![Action::Keep; 21];
        action[8..10].fill(Action::Discard);
        action[10] = Action::Investigate;
        action[11..13].fill(Action::Discard);
        // rdis == 4, rpdis == 2 -> 8 < 6 is false: kept.
        assert!(!clean_mmatch(&action, 10));
        // With one more unmatched record on each side rdis == 6 -> 8 < 8 is still false.
        action[7] = Action::Discard;
        action[13] = Action::Discard;
        assert!(!clean_mmatch(&action, 10));
        // rdis == 8 -> 8 < 10 holds: discarded.
        action[6] = Action::Discard;
        action[14] = Action::Discard;
        assert!(clean_mmatch(&action, 10));
    }

    #[test]
    fn run_of_only_multi_matches_is_kept() {
        let mut action = vec![Action::Keep; 40];
        action[10..30].fill(Action::Investigate);
        assert!(!clean_mmatch(&action, 20), "no unmatched record before the run");
    }
}
