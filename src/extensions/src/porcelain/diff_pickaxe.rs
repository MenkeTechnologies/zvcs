//! `diffcore-pickaxe.c` — the `-S`/`-G`/`--find-object` filter, and the pattern
//! type `-I`/`--ignore-matching-lines` shares with it.
//!
//! git has one `diffcore_pickaxe()` and every diff verb reaches it through
//! `diffcore_std()`, so the decision "does this pair match?" is written once here
//! rather than once per command. What each verb keeps to itself is only its own
//! pair representation: `diff`, `diff-index` and `diff-files` each carry a
//! different `Delta`, and they hand this module the two sides' bytes.
//!
//! ```c
//! void diffcore_pickaxe(struct diff_options *o)
//! {
//!         const char *needle = o->pickaxe;
//!         int opts = o->pickaxe_opts;
//!         regex_t regex, *regexp = NULL;
//!         kwset_t kws = NULL;
//!         pickaxe_fn fn;
//!
//!         if (opts & ~DIFF_PICKAXE_KIND_OBJFIND &&
//!             (!needle || !*needle))
//!                 BUG("should have needle under -G or -S");
//!         if (opts & (DIFF_PICKAXE_REGEX | DIFF_PICKAXE_KIND_G)) {
//!                 int cflags = REG_EXTENDED | REG_NEWLINE;
//!                 …
//! ```
//!
//! The two kinds differ in what they measure, which is the whole distinction the
//! manual draws between them: `-S` asks whether the *number of occurrences*
//! changed (`has_changes()` → `diff_grep()`'s counterpart `count_changes()`), so a
//! line that merely moves is not a hit; `-G` asks whether any line the diff
//! *touches* matches, so a move is.

use gix::hash::ObjectId;
use regex::bytes::Regex;

/// A search pattern: a literal substring (git's kwset path for a plain `-S`) or a
/// compiled regular expression (git's `-G`, `-I`, and `-S --pickaxe-regex`, all of
/// which call `regcomp` with `REG_EXTENDED | REG_NEWLINE`).
/// `Clone` because the history commands hand their whole option set to each patch
/// worker by value, and `-I<re>`'s compiled patterns travel with it. Cloning a
/// `regex::bytes::Regex` shares the compiled program behind an `Arc`, so a per-worker
/// copy costs a refcount rather than a recompile.
#[derive(Clone)]
pub(crate) enum Needle {
    Literal(Vec<u8>),
    Regex(Regex),
}

impl Needle {
    /// Whether `hay` contains a match — used by `-G` on each changed line and by `-I`.
    pub(crate) fn is_match(&self, hay: &[u8]) -> bool {
        match self {
            Needle::Literal(n) => contains(hay, n),
            Needle::Regex(re) => re.is_match(hay),
        }
    }

    /// Non-overlapping match count — used by `-S` to compare the two sides.
    pub(crate) fn count(&self, hay: &[u8]) -> usize {
        match self {
            Needle::Literal(n) => count_occurrences(hay, n),
            Needle::Regex(re) => re.find_iter(hay).count(),
        }
    }
}

/// Compile a `-G`/`-I`/`-S --pickaxe-regex` pattern the way git's `regcomp` does: on
/// bytes, without Unicode mode so `.` and the character classes carry git's C-locale
/// byte semantics, and with multi-line mode standing in for `REG_NEWLINE` since
/// matching is done a line at a time.
///
/// `Err` carries the text `regcomp_or_die()` (diffcore-pickaxe.c:219-228) hands to
/// `die("invalid regex: %s")`: the platform `regerror()` wording, which
/// [`super::line_log::ere_syntax_error`] reproduces for the syntax errors that have a
/// stable one — every caller compiles with `REG_EXTENDED` — and the engine's own
/// message for anything else.
pub(crate) fn compile_regex(pat: &[u8]) -> std::result::Result<Regex, String> {
    compile_regex_icase(pat, false)
}

/// [`compile_regex`] with `REG_ICASE` when `icase` — the flag `diffcore_pickaxe()` adds
/// for `DIFF_PICKAXE_IGNORE_CASE` (diffcore-pickaxe.c:244-245). Without Unicode mode the
/// fold is ASCII-only, as `REG_ICASE` is in git's C locale.
pub(crate) fn compile_regex_icase(pat: &[u8], icase: bool) -> std::result::Result<Regex, String> {
    let s = std::str::from_utf8(pat).map_err(|_| "invalid byte sequence in pattern".to_owned())?;
    regex::bytes::RegexBuilder::new(s)
        .unicode(false)
        .multi_line(true)
        .case_insensitive(icase)
        .build()
        .map_err(|e| match super::line_log::ere_syntax_error(s) {
            Some(text) => text.to_owned(),
            None => e.to_string(),
        })
}

/// A plain `-S<needle>` under `DIFF_PICKAXE_IGNORE_CASE` (`-i` /
/// `--regexp-ignore-case`, revision.c:2690-2692). `diffcore_pickaxe()` counts an
/// ASCII needle with a kwset over `tolower_trans_tbl`, and quotes any other needle
/// into a basic regex compiled `REG_NEWLINE | REG_ICASE` (diffcore-pickaxe.c:257-272).
/// Both are a case-insensitive literal count in git's C locale, i.e. ASCII-only
/// folding, which is what this byte-escaped, non-Unicode pattern matches.
pub(crate) fn literal_icase(needle: &[u8]) -> Regex {
    let mut pat = String::with_capacity(needle.len() * 4);
    for &b in needle {
        match b {
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' => pat.push(b as char),
            _ => pat.push_str(&format!("\\x{b:02x}")),
        }
    }
    regex::bytes::RegexBuilder::new(&pat)
        .unicode(false)
        .case_insensitive(true)
        .build()
        .expect("an escaped literal always compiles")
}

/// The needle `diffcore_pickaxe()` (diffcore-pickaxe.c:241-272) builds from `o->pickaxe`:
/// a `REG_EXTENDED | REG_NEWLINE` regex for `-G` and for `-S --pickaxe-regex`
/// (`regex`), a literal kwset count for a plain `-S`, and `REG_ICASE` / a
/// `tolower_trans_tbl` kwset on top of either under `DIFF_PICKAXE_IGNORE_CASE` —
/// the `-i` / `--regexp-ignore-case` revision option (revision.c:2690-2692).
///
/// `Err` is the text of `regcomp_or_die()`'s `die("invalid regex: %s")`. Every diff
/// verb compiles its needle here, so `-i` means the same thing in all of them.
pub(crate) fn compile_needle(pat: Vec<u8>, regex: bool, icase: bool) -> std::result::Result<Needle, String> {
    match (regex, icase) {
        (true, _) => compile_regex_icase(&pat, icase).map(Needle::Regex),
        (false, true) => Ok(Needle::Regex(literal_icase(&pat))),
        (false, false) => Ok(Needle::Literal(pat)),
    }
}

/// Occurrences of `needle` in `haystack`, counted without overlap, as git's kwset
/// does — `diffcore_count_changes()`'s `contains()` walks forward past each whole
/// match rather than by one byte, so `aa` occurs once in `aaa`, not twice.
///
/// The empty needle answers 0 rather than "one per position": git's `-S` callers
/// test `if (!len)` before ever asking, and `--find-object`'s empty case never
/// reaches here.
///
/// `memmem` is a vectorized substring search (git gets the same effect from its
/// kwset); the skip-forward fold turns its overlapping matches into git's
/// non-overlapping count. A byte-at-a-time loop reads a multi-megabyte blob far
/// more slowly for the same answer — see the equivalence test below.
pub(super) fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || needle.len() > haystack.len() {
        return 0;
    }
    memchr::memmem::find_iter(haystack, needle)
        .fold((0usize, 0usize), |(n, next), at| {
            if at >= next {
                (n + 1, at + needle.len())
            } else {
                (n, next)
            }
        })
        .0
}

pub(super) fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    count_occurrences(haystack, needle) > 0
}

/// Split into lines, each keeping its terminator, as xdiff records them.
pub(super) fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    data.split_inclusive(|&c| c == b'\n').collect()
}

/// Every line the diff between the two sides adds or removes, in hunk order.
///
/// `diff_grep()` runs its own `xdi_diff_outf()` over the pair with a zeroed
/// `xdemitconf_t` — context length 0 — rather than reusing whatever patch the
/// command is about to render, so the set of lines it sees does not depend on
/// `-U`, on the whitespace flags, or on whether a patch is being printed at all.
pub(super) fn for_each_changed_line(before: &[&[u8]], after: &[&[u8]], mut visit: impl FnMut(&[u8])) {
    use gix::diff::blob::{Algorithm, Diff, InternedInput};
    let one: Vec<u8> = before.concat();
    let two: Vec<u8> = after.concat();
    let input = InternedInput::new(one.as_slice(), two.as_slice());
    let diff = Diff::compute(Algorithm::Myers, &input);
    for hunk in diff.hunks() {
        for i in hunk.before.clone() {
            if let Some(line) = before.get(i as usize) {
                visit(line);
            }
        }
        for i in hunk.after.clone() {
            if let Some(line) = after.get(i as usize) {
                visit(line);
            }
        }
    }
}

/// git's `-G`: does any line the diff adds or removes match `needle`?
pub(super) fn changed_lines_hit(one: &[u8], two: &[u8], needle: &Needle) -> bool {
    let before = split_lines(one);
    let after = split_lines(two);
    let mut hit = false;
    for_each_changed_line(&before, &after, |line| {
        if needle.is_match(line) {
            hit = true;
        }
    });
    hit
}

/// `pickaxe_match()`'s `DIFF_PICKAXE_KIND_OBJFIND` branch (`diffcore-pickaxe.c`):
///
/// ```c
/// if (o->objfind) {
///         return  (DIFF_FILE_VALID(p->one) &&
///                  oidset_contains(o->objfind, &p->one->oid)) ||
///                 (DIFF_FILE_VALID(p->two) &&
///                  oidset_contains(o->objfind, &p->two->oid));
/// }
/// ```
///
/// It reads `p->one->oid` / `p->two->oid` as *recorded*, never hashing anything, so
/// a worktree post-image git has not hashed carries the null id and cannot match —
/// which is why `git diff --find-object=<hash of the working-tree file>` finds
/// nothing while the same id on the index side does. `None` is `!DIFF_FILE_VALID`,
/// the side that does not exist at all.
///
/// One expression, shared by all three diff verbs, because each of them keeps its
/// own pair representation and only the two ids are common.
pub(crate) fn objfind_hit(ids: &[ObjectId], one: Option<ObjectId>, two: Option<ObjectId>) -> bool {
    one.is_some_and(|id| ids.contains(&id)) || two.is_some_and(|id| ids.contains(&id))
}

/// `-S<string>` counts occurrences; `-G<pattern>` looks at the changed lines;
/// `--find-object=<id>` keeps a pair that touches one of the named object ids.
pub(super) enum Kind {
    /// `-S`: a literal count by default, a regex count under `--pickaxe-regex`.
    Occurrences(Needle),
    /// `-G`: a pattern over the added and removed lines.
    Grep(Needle),
    /// `--find-object=<id>`: `pickaxe_match()`'s `DIFF_PICKAXE_KIND_OBJFIND` branch.
    ObjFind(Vec<ObjectId>),
}

impl Kind {
    /// `pickaxe_match()` for the two content kinds, given each side's bytes — `None`
    /// for a side that does not exist, which is git's `!DIFF_FILE_VALID` and reads
    /// as an empty buffer in both `has_changes()` and `diff_grep()`.
    ///
    /// ```c
    /// static int has_changes(mmfile_t *one, mmfile_t *two,
    ///                        struct diff_options *o,
    ///                        regex_t *regexp, kwset_t kws)
    /// {
    ///         unsigned int one_contains = one ? contains(one, regexp, kws) : 0;
    ///         unsigned int two_contains = two ? contains(two, regexp, kws) : 0;
    ///         return one_contains != two_contains;
    /// }
    /// ```
    ///
    /// `ObjFind` never reaches here: it is answered from the recorded ids by
    /// [`objfind_hit`] before either filespec would be filled.
    pub(super) fn content_hit(&self, one: Option<&[u8]>, two: Option<&[u8]>) -> bool {
        match self {
            Kind::ObjFind(_) => false,
            Kind::Occurrences(needle) => {
                let a = one.map(|b| needle.count(b)).unwrap_or(0);
                let b = two.map(|b| needle.count(b)).unwrap_or(0);
                a != b
            }
            Kind::Grep(needle) => match (one, two) {
                (None, None) => false,
                // `diff_grep()`'s two one-sided shortcuts: with only one filespec
                // valid the whole of it is either added or removed, so the pattern
                // is run over the buffer rather than over a diff against nothing.
                (None, Some(t)) | (Some(t), None) => needle.is_match(t),
                (Some(a), Some(b)) => changed_lines_hit(a, b, needle),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::count_occurrences;

    /// The byte-at-a-time walk this used to be, kept as the reference the fast
    /// path is measured against.
    fn naive(haystack: &[u8], needle: &[u8]) -> usize {
        if needle.is_empty() || needle.len() > haystack.len() {
            return 0;
        }
        let mut count = 0;
        let mut at = 0;
        while at + needle.len() <= haystack.len() {
            if &haystack[at..at + needle.len()] == needle {
                count += 1;
                at += needle.len();
            } else {
                at += 1;
            }
        }
        count
    }

    /// Overlapping candidates are the whole point: `aa` in `aaaa` is 2, not 3.
    #[test]
    fn non_overlapping_like_kwset() {
        assert_eq!(count_occurrences(b"aaaa", b"aa"), 2);
        assert_eq!(count_occurrences(b"aaa", b"aa"), 1);
        assert_eq!(count_occurrences(b"abababa", b"aba"), 2);
        assert_eq!(count_occurrences(b"", b"a"), 0);
        assert_eq!(count_occurrences(b"a", b""), 0);
        assert_eq!(count_occurrences(b"a", b"aa"), 0);
    }

    /// Exhaustive equivalence with the reference walk over a small alphabet, so
    /// the swap to `memmem` cannot have changed any answer.
    #[test]
    fn agrees_with_the_naive_walk() {
        let alphabet = b"ab";
        let mut hays: Vec<Vec<u8>> = vec![Vec::new()];
        for _ in 0..8 {
            let mut next = Vec::new();
            for h in &hays {
                for &c in alphabet {
                    let mut v = h.clone();
                    v.push(c);
                    next.push(v);
                }
            }
            hays.extend(next);
        }
        let needles: Vec<&[u8]> = vec![b"a", b"b", b"aa", b"ab", b"aba", b"abab", b"bbbb"];
        for h in &hays {
            for n in &needles {
                assert_eq!(
                    count_occurrences(h, n),
                    naive(h, n),
                    "haystack {h:?} needle {n:?}"
                );
            }
        }
    }
}
