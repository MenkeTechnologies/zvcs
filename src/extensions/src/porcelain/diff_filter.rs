//! `--diff-filter=<letters>` — `diff_opt_diff_filter()` and
//! `diffcore_apply_filter()` (diff.c:5470-5500, 7303-7341).
//!
//! git keeps two bit sets on `struct diff_options`: `filter` for the uppercase
//! letters (statuses to keep) and `filter_not` for their lowercase spellings
//! (statuses to drop). Every occurrence of the option ors into them, so
//! `--diff-filter=A --diff-filter=D` selects both. `diff_setup_done()`
//! (diff.c:5370-5374) then folds the two together: an exclusion-only value starts
//! from "every status but `*`" and subtracts, which is what makes `--diff-filter=d`
//! mean "everything except deletions".
//!
//! Two of the ten letters are not statuses a pair can carry:
//!
//!   * `B` (`DIFF_STATUS_FILTER_BROKEN`) selects a `M` pair that `-B` gave a score,
//!     and plain `M` then stops selecting it — `match_filter()` splits the modified
//!     status on `p->score` rather than testing both bits.
//!   * `*` (`DIFF_STATUS_FILTER_AON`) is all-or-none: if any pair in the queue
//!     matches, the whole queue is kept; if none does, the whole queue is dropped.
//!
//! This is the shared implementation; [`super::diff`] carries an older partial
//! copy (`diff::diff_filter_selected`) that knows neither `B` nor `*`.

/// The two accumulated bit sets, in `diff_options` order.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct Filter {
    /// `options->filter`.
    include: u32,
    /// `options->filter_not`.
    exclude: u32,
}

/// `diff_status_letters[]` (diff.c:5182-5194), in the order `prepare_filter_bits()`
/// assigns their bits. Only membership matters here, not the bit values.
const LETTERS: &[u8] = b"ACDMRTXU*B";

/// The bit `prepare_filter_bits()` gives one status letter, or `None` when the
/// letter names no change class at all.
fn bit(letter: u8) -> Option<u32> {
    LETTERS.iter().position(|&c| c == letter).map(|i| 1 << i)
}

impl Filter {
    /// `diff_opt_diff_filter()`: fold one `--diff-filter=<letters>` value into the
    /// accumulated sets. The error is the offending letter, which the caller reports
    /// as `unknown change class '<c>' in --diff-filter=<value>` at exit 129.
    pub(crate) fn accumulate(&mut self, value: &str) -> Result<(), char> {
        for ch in value.chars() {
            let negate = ch.is_ascii_lowercase();
            let up = ch.to_ascii_uppercase();
            // `filter_bit[optch]` is indexed by a `char`, so anything past `Z` — a
            // multi-byte character included — reads as no bit at all.
            let Some(b) = u8::try_from(up).ok().and_then(bit) else {
                return Err(ch);
            };
            if negate {
                self.exclude |= b;
            } else {
                self.include |= b;
            }
        }
        Ok(())
    }

    /// `diff_setup_done()`'s fold (diff.c:5370-5374): an exclusion with no inclusion
    /// beside it starts from every status except `*`, then subtracts.
    fn resolved(self) -> u32 {
        if self.exclude == 0 {
            return self.include;
        }
        let base = if self.include == 0 {
            !bit(b'*').expect("'*' is a status letter")
        } else {
            self.include
        };
        base & !self.exclude
    }

    /// Whether `diffcore_apply_filter()` would do anything at all: its first line is
    /// `if (!options->filter) return;`.
    fn active(self) -> bool {
        self.resolved() != 0
    }

    /// `match_filter()` (diff.c:7292-7301) for one pair, given its status letter and
    /// the `-B` score a modified pair may carry.
    fn matches(self, status: u8, score: Option<u32>) -> bool {
        let f = self.resolved();
        let tst = |c: u8| bit(c).is_some_and(|b| f & b != 0);
        if status == b'M' {
            return if score.is_some() { tst(b'B') } else { tst(b'M') };
        }
        tst(status)
    }
}

/// `diffcore_apply_filter()` over a queue, as `(status, break-score)` per pair:
/// returns the keep/drop decision for each in queue order.
///
/// `*` short-circuits the whole queue — one match keeps everything, no match drops
/// everything — which is why this answers for the queue rather than per pair.
pub(crate) fn apply(filter: Filter, pairs: &[(u8, Option<u32>)]) -> Vec<bool> {
    if !filter.active() {
        return vec![true; pairs.len()];
    }
    let matched = |p: &(u8, Option<u32>)| filter.matches(p.0, p.1);
    if bit(b'*').is_some_and(|b| filter.resolved() & b != 0) {
        let any = pairs.iter().any(matched);
        return vec![any; pairs.len()];
    }
    pairs.iter().map(matched).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(value: &str) -> Filter {
        let mut filter = Filter::default();
        filter.accumulate(value).expect("value is well-formed");
        filter
    }

    /// The exclusion-only fold: `diff_setup_done()` seeds "everything but `*`" and
    /// subtracts, so a lone `d` keeps additions and modifications alike.
    #[test]
    fn lowercase_only_keeps_the_rest() {
        assert_eq!(
            apply(f("d"), &[(b'A', None), (b'D', None), (b'M', None)]),
            vec![true, false, true]
        );
    }

    /// `B` and `M` split the modified status on `p->score`, so neither letter selects
    /// the other's pairs.
    #[test]
    fn broken_and_modified_are_disjoint() {
        let pairs = [(b'M', None), (b'M', Some(80))];
        assert_eq!(apply(f("M"), &pairs), vec![true, false]);
        assert_eq!(apply(f("B"), &pairs), vec![false, true]);
    }

    /// `*` is all-or-none over the whole queue.
    #[test]
    fn aon_is_decided_for_the_queue() {
        let pairs = [(b'A', None), (b'M', None)];
        assert_eq!(apply(f("A*"), &pairs), vec![true, true]);
        assert_eq!(apply(f("D*"), &pairs), vec![false, false]);
    }

    /// Every occurrence ors in, and an unknown letter names itself.
    #[test]
    fn accumulates_and_rejects() {
        let mut filter = Filter::default();
        filter.accumulate("A").expect("A is a status letter");
        filter.accumulate("D").expect("D is a status letter");
        assert_eq!(
            apply(filter, &[(b'A', None), (b'D', None), (b'M', None)]),
            vec![true, true, false]
        );
        assert_eq!(Filter::default().accumulate("Z"), Err('Z'));
        assert_eq!(Filter::default().accumulate("Aé"), Err('é'));
    }
}
