//! git's own pathspec matcher — `match_pathspec_item()`, `do_match_pathspec()`
//! and `match_pathspec_with_flags()` (dir.c:387-593) — together with the walk
//! helpers that read a pathspec's items directly: `common_prefix_len()`
//! (dir.c:204-247), `simplify_away()` (dir.c:2223-2251) and
//! `exclude_matches_pathspec()` (dir.c:2264-2294).
//!
//! [`Search::pattern_matching_relative_path()`] answers "does it match" the gix
//! way. `read_directory()` needs git's graded answer instead — `MATCHED_EXACTLY`,
//! `MATCHED_FNMATCH`, `MATCHED_RECURSIVELY` or, under `DO_MATCH_LEADING_PATHSPEC`,
//! `MATCHED_RECURSIVELY_LEADING_PATHSPEC` — because `treat_directory()` decides
//! between listing a directory and descending into it on exactly that grade.
//!
//! Each item is rebuilt from the normalized gix pattern the way `parse_pathspec()`
//! lays out a `struct pathspec_item`: `match` is the root-relative path with the
//! trailing `/` a directory-only spec was written with, `prefix` is the length of
//! the command's prefix inside it (with its slash), and `nowildcard_len` is
//! `simple_length(match)`, raised to `prefix` for anything but `:(literal)`
//! (pathspec.c:516-522).
//!
//! Not ported: `:(prefix:<n>)`-style `max_depth`, which only `grep` sets.
use bstr::{BStr, BString, ByteSlice};

use crate::{MagicSignature, Search, SearchMode};

/// `MATCHED_RECURSIVELY` (dir.h:388).
pub const MATCHED_RECURSIVELY: u8 = 1;
/// `MATCHED_RECURSIVELY_LEADING_PATHSPEC` (dir.h:389).
pub const MATCHED_RECURSIVELY_LEADING_PATHSPEC: u8 = 2;
/// `MATCHED_FNMATCH` (dir.h:390).
pub const MATCHED_FNMATCH: u8 = 3;
/// `MATCHED_EXACTLY` (dir.h:391).
pub const MATCHED_EXACTLY: u8 = 4;

/// `DO_MATCH_EXCLUDE` (dir.c:371).
pub const DO_MATCH_EXCLUDE: u32 = 1 << 0;
/// `DO_MATCH_DIRECTORY` (dir.c:372).
pub const DO_MATCH_DIRECTORY: u32 = 1 << 1;
/// `DO_MATCH_LEADING_PATHSPEC` (dir.c:373).
pub const DO_MATCH_LEADING_PATHSPEC: u32 = 1 << 2;

/// One `struct pathspec_item`, as far as the matcher reads it.
struct Item {
    /// `item->match`.
    matcher: BString,
    /// `item->prefix`.
    prefix: usize,
    /// `item->nowildcard_len`.
    nowildcard_len: usize,
    icase: bool,
    exclude: bool,
    /// `PATHSPEC_GLOB`: `wildmatch()` with `WM_PATHNAME`.
    glob: bool,
}

/// `simple_length()` (dir.c:681-690): the length of the leading run of bytes
/// that are not glob-special (`*`, `?`, `[`, `\`).
fn simple_length(s: &[u8]) -> usize {
    s.iter()
        .position(|b| matches!(b, b'*' | b'?' | b'[' | b'\\'))
        .unwrap_or(s.len())
}

/// `ps_strncmp()` (dir.c:95-103): equal over the first `n` bytes, folding ASCII
/// case under `:(icase)`. A side shorter than `n` compares as if NUL-terminated.
fn ps_strneq(icase: bool, a: &[u8], b: &[u8], n: usize) -> bool {
    let a = &a[..n.min(a.len())];
    let b = &b[..n.min(b.len())];
    if icase { a.eq_ignore_ascii_case(b) } else { a == b }
}

impl Search {
    /// The items in the order they were given — gix keeps its patterns sorted
    /// exclusions first, git keeps `pathspec->items` in argument order, and
    /// `common_prefix_len()` measures against `items[0]`.
    fn git_items(&self) -> Vec<(usize, Item)> {
        let mut order: Vec<usize> = (0..self.patterns.len()).collect();
        order.sort_by_key(|&idx| self.patterns[idx].sequence_number);
        order
            .into_iter()
            .map(|idx| (idx, &self.patterns[idx]))
            .map(|(idx, mapping)| {
                let pattern = &mapping.value.pattern;
                let mut matcher = if pattern.is_nil() {
                    BString::default()
                } else {
                    pattern.path.clone()
                };
                if !matcher.is_empty() && pattern.signature.contains(MagicSignature::MUST_BE_DIR) {
                    matcher.push(b'/');
                }
                let prefix = if pattern.prefix_len == 0 {
                    0
                } else {
                    (pattern.prefix_len + 1).min(matcher.len())
                };
                let literal = pattern.search_mode == SearchMode::Literal;
                let nowildcard_len = if literal {
                    matcher.len()
                } else {
                    simple_length(&matcher).max(prefix)
                };
                (
                    idx,
                    Item {
                        nowildcard_len,
                        prefix,
                        icase: pattern.signature.contains(MagicSignature::ICASE),
                        exclude: pattern.is_excluded(),
                        glob: pattern.search_mode == SearchMode::PathAwareGlob,
                        matcher,
                    },
                )
            })
            .collect()
    }

    /// `match_pathspec_with_flags()` (dir.c:578-593) with a zero `prefix` and no
    /// `seen[]`, as `read_directory()` calls it. `name` is matched as given — a
    /// directory keeps the trailing `/` its caller put there.
    ///
    /// `attributes` is consulted for `:(attr:…)` items exactly as in
    /// [`Search::pattern_matching_relative_path()`].
    pub fn match_pathspec_with_flags(
        &mut self,
        name: &BStr,
        flags: u32,
        attributes: &mut dyn FnMut(
            &BStr,
            gix_glob::pattern::Case,
            bool,
            &mut gix_attributes::search::Outcome,
        ) -> bool,
    ) -> u8 {
        let items = self.git_items();
        let positive = self.do_match_pathspec(&items, name, flags, attributes);
        if !items.iter().any(|(_, item)| item.exclude) || positive == 0 {
            return positive;
        }
        let negative = self.do_match_pathspec(&items, name, flags | DO_MATCH_EXCLUDE, attributes);
        if negative != 0 { 0 } else { positive }
    }

    /// `match_pathspec()` (dir.c:595-602).
    pub fn match_pathspec(
        &mut self,
        name: &BStr,
        is_dir: bool,
        attributes: &mut dyn FnMut(
            &BStr,
            gix_glob::pattern::Case,
            bool,
            &mut gix_attributes::search::Outcome,
        ) -> bool,
    ) -> u8 {
        let flags = if is_dir { DO_MATCH_DIRECTORY } else { 0 };
        self.match_pathspec_with_flags(name, flags, attributes)
    }

    /// `do_match_pathspec()` (dir.c:513-576).
    ///
    /// `parse_pathspec()` appends a match-everything item when every element is
    /// an exclusion (pathspec.c:630-640); gix keeps no such item, so a search
    /// with no positive item answers for it — `MATCHED_RECURSIVELY`, what
    /// `match_pathspec_item()` returns for an empty `match`.
    fn do_match_pathspec(
        &mut self,
        items: &[(usize, Item)],
        name: &BStr,
        flags: u32,
        attributes: &mut dyn FnMut(
            &BStr,
            gix_glob::pattern::Case,
            bool,
            &mut gix_attributes::search::Outcome,
        ) -> bool,
    ) -> u8 {
        if items.is_empty() {
            return MATCHED_RECURSIVELY;
        }
        let exclude = flags & DO_MATCH_EXCLUDE != 0;
        if !exclude && items.iter().all(|(_, item)| item.exclude) {
            return MATCHED_RECURSIVELY;
        }
        let mut retval = 0;
        for (idx, item) in items.iter().rev() {
            if item.exclude != exclude {
                continue;
            }
            let how = self.match_pathspec_item(*idx, item, name, flags, attributes);
            retval = retval.max(how);
        }
        retval
    }

    /// `match_pathspec_item()` (dir.c:387-492) with a zero `prefix`.
    fn match_pathspec_item(
        &mut self,
        idx: usize,
        item: &Item,
        name: &BStr,
        flags: u32,
        attributes: &mut dyn FnMut(
            &BStr,
            gix_glob::pattern::Case,
            bool,
            &mut gix_attributes::search::Outcome,
        ) -> bool,
    ) -> u8 {
        let matcher = item.matcher.as_slice();
        let matchlen = matcher.len();
        let namelen = name.len();

        if item.prefix != 0 && item.icase && !ps_strneq(false, matcher, name, item.prefix) {
            return 0;
        }

        // `match_pathspec_attrs()` (pathspec.c:725-761) checks the name without
        // any trailing slash a directory was handed in with.
        let mapping = &mut self.patterns[idx];
        if let Some(attrs) = mapping.value.attrs_match.as_mut() {
            let is_dir = name.last() == Some(&b'/');
            let path = if is_dir { name[..namelen - 1].as_bstr() } else { name };
            if !attributes(path, gix_glob::pattern::Case::Sensitive, is_dir, attrs) {
                attrs.reset();
            }
            for (actual, expected) in attrs.iter_selected().zip(mapping.value.pattern.attributes.iter()) {
                if actual.assignment != expected.as_ref() {
                    return 0;
                }
            }
        }

        if matcher.is_empty() {
            return MATCHED_RECURSIVELY;
        }

        if matchlen <= namelen && ps_strneq(item.icase, matcher, name, matchlen) {
            if matchlen == namelen {
                return MATCHED_EXACTLY;
            }
            if matcher[matchlen - 1] == b'/' || name[matchlen] == b'/' {
                return MATCHED_RECURSIVELY;
            }
        } else if flags & DO_MATCH_DIRECTORY != 0
            && matcher[matchlen - 1] == b'/'
            && namelen == matchlen - 1
            && ps_strneq(item.icase, matcher, name, namelen)
        {
            return MATCHED_EXACTLY;
        }

        if item.nowildcard_len < matchlen && git_fnmatch(item, matcher, name, item.nowildcard_len) {
            return MATCHED_FNMATCH;
        }

        if flags & DO_MATCH_LEADING_PATHSPEC != 0 && flags & DO_MATCH_EXCLUDE == 0 {
            let offset = usize::from(name.last() == Some(&b'/'));
            if namelen < matchlen && matcher[namelen - offset] == b'/' && ps_strneq(item.icase, matcher, name, namelen) {
                return MATCHED_RECURSIVELY_LEADING_PATHSPEC;
            }
            if item.nowildcard_len < matchlen && !ps_strneq(item.icase, matcher, name, item.nowildcard_len) {
                return 0;
            }
            if item.nowildcard_len == matchlen {
                return 0;
            }
            return MATCHED_RECURSIVELY_LEADING_PATHSPEC;
        }
        0
    }

    /// `common_prefix_len()` (dir.c:204-247): the longest leading *directory*
    /// every positive item shares, up to its first wildcard — or, under
    /// `:(icase)`, up to the command's prefix only.
    pub fn git_common_prefix_len(&self) -> usize {
        let items = self.git_items();
        let Some((_, first)) = items.first() else {
            return 0;
        };
        let mut max = 0;
        for (n, (_, item)) in items.iter().enumerate() {
            if item.exclude {
                continue;
            }
            let item_len = if item.icase { item.prefix } else { item.nowildcard_len };
            let mut len = 0;
            let mut i = 0;
            while i < item_len && (n == 0 || i < max) {
                let c = item.matcher[i];
                if first.matcher.get(i) != Some(&c) {
                    break;
                }
                if c == b'/' {
                    len = i + 1;
                }
                i += 1;
            }
            if n == 0 || len < max {
                max = len;
                if max == 0 {
                    break;
                }
            }
        }
        max
    }

    /// The directory [`Search::git_common_prefix_len()`] measures, as
    /// `fill_directory()` passes it on: `pathspec->items[0].match` cut to that length.
    pub fn git_common_prefix(&self) -> BString {
        let len = self.git_common_prefix_len();
        self.git_items()
            .first()
            .map(|(_, item)| BString::from(&item.matcher[..len]))
            .unwrap_or_default()
    }

    /// `simplify_away()` (dir.c:2223-2251): `true` if no item can match anything
    /// at or below `path`, judged on the literal part of each item alone.
    pub fn simplify_away(&self, path: &BStr) -> bool {
        let items = self.git_items();
        if items.is_empty() || items.iter().all(|(_, item)| item.exclude) {
            return false;
        }
        !items.iter().any(|(_, item)| {
            let len = item.nowildcard_len.min(path.len());
            ps_strneq(item.icase, &item.matcher, path, len)
        })
    }

    /// `exclude_matches_pathspec()` (dir.c:2264-2294): whether `path` is named by
    /// an item outright, or is a leading directory of one.
    pub fn exclude_matches_pathspec(&self, path: &BStr) -> bool {
        let pathlen = path.len();
        self.git_items().iter().any(|(_, item)| {
            let len = item.nowildcard_len;
            (len == pathlen && ps_strneq(item.icase, &item.matcher, path, pathlen))
                || (len > pathlen && item.matcher[pathlen] == b'/' && ps_strneq(item.icase, &item.matcher, path, pathlen))
        })
    }
}

/// `git_fnmatch()` (dir.c:105-128): the first `prefix` bytes literally, the rest
/// through `wildmatch()` — with `WM_PATHNAME` only under `:(glob)`. The
/// `PATHSPEC_ONESTAR` shortcut is the same answer computed faster, so it is not
/// reproduced.
fn git_fnmatch(item: &Item, pattern: &[u8], string: &[u8], prefix: usize) -> bool {
    if prefix > 0 {
        if !ps_strneq(item.icase, pattern, string, prefix) || string.len() < prefix {
            return false;
        }
    }
    let mut mode = gix_glob::wildmatch::Mode::empty();
    if item.glob {
        mode |= gix_glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL;
    }
    if item.icase {
        mode |= gix_glob::wildmatch::Mode::IGNORE_CASE;
    }
    gix_glob::wildmatch(pattern[prefix..].as_bstr(), string[prefix..].as_bstr(), mode)
}
