//! `-L<range>:<file>` for `git log` and `git show`: line-level history.
//!
//! Three of git's files meet here, and each is ported as its own section below:
//!
//!   * `line-range.c` — the `-L` argument grammar (`parse_loc`,
//!     `parse_range_funcname`, `parse_range_arg`, `skip_range_arg`). It resolves
//!     one `<start>,<end>` / `:<funcname>` spec against the blob of the commit the
//!     walk starts from, which is why a bad range is fatal before any traversal.
//!   * `line-log.c` — the range-set algebra (union, difference, shift, map across a
//!     diff) and the traversal that carries a set of tracked ranges backward through
//!     history, keeping a commit only when one of its hunks touched a tracked range.
//!   * `diff.c`'s `line_range_callback` — the output filter that clips each file's
//!     unified diff back to the tracked ranges and re-headers the fragments.
//!
//! Ranges are half-open and 0-based (`[start, end)`), exactly as git stores them
//! after `parse_lines()` decrements the human-entered start.
//!
//! A tracked range follows its file across a `git mv`: when the path-limited diff
//! shows the file appearing from nowhere, `queue_diffs()` reruns the diff over the
//! whole tree and puts it through [`diffcore_rename`], exactly as git does.
//!
//! Deviations, surfaced rather than faked:
//!
//!   * A regular expression that fails to compile reports git's message text for
//!     the balance/repetition errors this module checks for and a generic
//!     `invalid regular expression` otherwise; the exact wording of the rest comes
//!     from the platform's `regerror()` and is not reproducible.

use gix::bstr::BString;
use gix::hash::ObjectId;
use gix::objs::tree::EntryKind;
use std::collections::HashMap;

use super::diff::{def_ff, func_line};
use super::diffcore_rename;

/// A fatal `-L` diagnostic. The caller prints `fatal: {0}` and exits 128, which is
/// what git's `die()` does.
pub(crate) struct Fatal(pub String);

type R<T> = std::result::Result<T, Fatal>;

fn die<T>(msg: impl Into<String>) -> R<T> {
    Err(Fatal(msg.into()))
}

// ---------------------------------------------------------------------------
// range sets (line-log.c)
// ---------------------------------------------------------------------------

/// git's `struct range`: a half-open, 0-based line interval `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Range {
    pub start: i64,
    pub end: i64,
}

/// git's `struct range_set`: non-empty, sorted, non-overlapping intervals.
#[derive(Clone, Debug, Default)]
pub(crate) struct RangeSet {
    pub ranges: Vec<Range>,
}

impl RangeSet {
    fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// `range_set_append_unsafe`: tack a range on at the end without the sortedness
    /// assertion, for the input-collection phase that `sort_and_merge` cleans up.
    fn append_unsafe(&mut self, a: i64, b: i64) {
        self.ranges.push(Range { start: a, end: b });
    }

    /// `sort_and_merge_range_set`: establish the invariants on a set built from raw
    /// user input — sort by start, drop empty ranges, merge overlapping ones.
    fn sort_and_merge(&mut self) {
        self.ranges.sort_by_key(|r| r.start);
        let mut out: Vec<Range> = Vec::with_capacity(self.ranges.len());
        for r in &self.ranges {
            if r.start == r.end {
                continue;
            }
            match out.last_mut() {
                Some(last) if r.start <= last.end => {
                    if last.end < r.end {
                        last.end = r.end;
                    }
                }
                _ => out.push(*r),
            }
        }
        self.ranges = out;
    }

    /// `range_set_union`: merge two sets, consolidating overlapping and adjacent
    /// ranges and dropping empty ones.
    fn union(a: &RangeSet, b: &RangeSet) -> RangeSet {
        let mut out = RangeSet::default();
        let (mut i, mut j) = (0usize, 0usize);
        while i < a.ranges.len() || j < b.ranges.len() {
            let new_range = if i < a.ranges.len() && j < b.ranges.len() {
                let (ra, rb) = (a.ranges[i], b.ranges[j]);
                if ra.start < rb.start || (ra.start == rb.start && ra.end < rb.end) {
                    i += 1;
                    ra
                } else {
                    j += 1;
                    rb
                }
            } else if i < a.ranges.len() {
                i += 1;
                a.ranges[i - 1]
            } else {
                j += 1;
                b.ranges[j - 1]
            };
            if new_range.start == new_range.end {
                continue;
            }
            match out.ranges.last_mut() {
                Some(last) if last.end >= new_range.start => {
                    if last.end < new_range.end {
                        last.end = new_range.end;
                    }
                }
                _ => out.ranges.push(new_range),
            }
        }
        out
    }

    /// `range_set_difference` (`out = a \ b`): drop from the interesting ranges the
    /// parts the commit itself is responsible for.
    fn difference(a: &RangeSet, b: &RangeSet) -> RangeSet {
        let mut out = RangeSet::default();
        let mut j = 0usize;
        for r in &a.ranges {
            let mut start = r.start;
            let end = r.end;
            while start < end {
                while j < b.ranges.len() && start >= b.ranges[j].end {
                    j += 1;
                }
                if j >= b.ranges.len() || end <= b.ranges[j].start {
                    out.ranges.push(Range { start, end });
                    break;
                }
                if start >= b.ranges[j].start {
                    start = b.ranges[j].end;
                } else if end > b.ranges[j].start {
                    if start < b.ranges[j].start {
                        out.ranges.push(Range { start, end: b.ranges[j].start });
                    }
                    start = b.ranges[j].end;
                }
            }
        }
        out
    }

    /// `range_set_shift_diff`: slide the untouched ranges by the net line count the
    /// diff's earlier hunks added or removed, putting them on the parent's numbering.
    fn shift_diff(rs: &RangeSet, diff: &DiffRanges) -> RangeSet {
        let mut out = RangeSet::default();
        let mut j = 0usize;
        let mut offset: i64 = 0;
        for src in &rs.ranges {
            while j < diff.target.ranges.len() && src.start >= diff.target.ranges[j].start {
                offset += (diff.parent.ranges[j].end - diff.parent.ranges[j].start)
                    - (diff.target.ranges[j].end - diff.target.ranges[j].start);
                j += 1;
            }
            out.ranges.push(Range { start: src.start + offset, end: src.end + offset });
        }
        out
    }

    /// `range_set_map_across_diff`: the target commit takes the blame for every
    /// touched hunk, so those ranges are replaced by their parent-side counterparts
    /// and the rest is shifted onto the parent's line numbering.
    fn map_across_diff(rs: &RangeSet, diff: &DiffRanges) -> (RangeSet, DiffRanges) {
        let touched = diff.filter_touched(rs);
        let tmp1 = RangeSet::difference(rs, &touched.target);
        let tmp2 = RangeSet::shift_diff(&tmp1, diff);
        (RangeSet::union(&tmp2, &touched.parent), touched)
    }
}

/// git's `struct diff_ranges`: the parent-side and target-side line spans of every
/// hunk of one file's diff, in step with each other.
#[derive(Clone, Debug, Default)]
struct DiffRanges {
    parent: RangeSet,
    target: RangeSet,
}

fn ranges_overlap(a: &Range, b: &Range) -> bool {
    !(a.end <= b.start || b.end <= a.start)
}

impl DiffRanges {
    /// `diff_ranges_filter_touched`: the hunks whose target side overlaps at least
    /// one interesting range.
    fn filter_touched(&self, rs: &RangeSet) -> DiffRanges {
        let mut out = DiffRanges::default();
        let mut j = 0usize;
        for i in 0..self.target.ranges.len() {
            while self.target.ranges[i].start >= rs.ranges[j].end {
                j += 1;
                if j == rs.ranges.len() {
                    return out;
                }
            }
            if ranges_overlap(&self.target.ranges[i], &rs.ranges[j]) {
                out.parent.ranges.push(self.parent.ranges[i]);
                out.target.ranges.push(self.target.ranges[i]);
            }
        }
        out
    }
}

/// `collect_diff`: the change script of `parent` against `target` with zero context
/// and no inter-hunk merging, as one parent-side/target-side range pair per hunk.
///
/// git zeroes its `xpparam_t` here, so this diff runs plain Myers with neither the
/// indent heuristic nor any whitespace flag — deliberately independent of how the
/// same pair will later be *rendered*.
fn collect_diff(parent: &[u8], target: &[u8]) -> DiffRanges {
    use gix::diff::blob::{Algorithm, Diff, InternedInput};

    let before = super::diff::byte_lines(parent);
    let after = super::diff::byte_lines(target);
    let mut input: InternedInput<Vec<u8>> = InternedInput::default();
    input.update_before(before.iter().map(|l| l.to_vec()));
    input.update_after(after.iter().map(|l| l.to_vec()));

    let mut diff = Diff::compute(Algorithm::Myers, &input);
    diff.postprocess_no_heuristic(&input);

    let mut out = DiffRanges::default();
    for hunk in diff.hunks() {
        out.parent.append_unsafe(hunk.before.start as i64, hunk.before.end as i64);
        out.target.append_unsafe(hunk.after.start as i64, hunk.after.end as i64);
    }
    out
}

// ---------------------------------------------------------------------------
// tracked files (line-log.c's `struct line_log_data`)
// ---------------------------------------------------------------------------

/// One side of a file pair: the object and mode, or `None` when the path is absent
/// on that side (a creation or a deletion).
pub(crate) type Side = Option<(ObjectId, EntryKind)>;

/// git's `rg->pair`: the file pair whose diff took the blame for a tracked range at
/// this commit, kept for the output pass.
#[derive(Clone, Debug)]
pub(crate) struct Pair {
    /// git's `pair->two->path`: the path at this commit.
    pub path: BString,
    /// git's `pair->one->path`. Differs from `path` only for a detected rename,
    /// which is how a tracked range follows a file across `git mv`.
    pub old_path: BString,
    pub old: Side,
    pub new: Side,
}

impl Pair {
    fn same_path(path: BString, old: Side, new: Side) -> Self {
        Pair { old_path: path.clone(), path, old, new }
    }

    /// `true` when detection matched this destination to a differently-named source.
    pub fn renamed(&self) -> bool {
        self.old_path != self.path
    }
}

/// git's `struct line_log_data`: one tracked path, its live ranges, and the pair
/// that made the commit holding this record interesting.
#[derive(Clone, Debug)]
pub(crate) struct FileRanges {
    pub path: BString,
    pub ranges: RangeSet,
    pub pair: Option<Pair>,
}

/// git's `line_log_data` list, kept sorted by path as `line_log_data_insert` does.
pub(crate) type Tracked = Vec<FileRanges>;

/// `line_log_data_insert`: extend an existing path's ranges, or splice a new record
/// into the sorted list.
fn insert(list: &mut Tracked, path: BString, begin: i64, end: i64) {
    match list.binary_search_by(|p| p.path.cmp(&path)) {
        Ok(i) => list[i].ranges.append_unsafe(begin, end),
        Err(i) => list.insert(
            i,
            FileRanges {
                path,
                ranges: RangeSet { ranges: vec![Range { start: begin, end }] },
                pair: None,
            },
        ),
    }
}

/// `line_log_data_merge`: union two per-path range lists, both sorted by path.
fn merge(a: &Tracked, b: &Tracked) -> Tracked {
    let mut out: Tracked = Vec::with_capacity(a.len().max(b.len()));
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() || j < b.len() {
        let cmp = match (a.get(i), b.get(j)) {
            (None, _) => std::cmp::Ordering::Greater,
            (_, None) => std::cmp::Ordering::Less,
            (Some(x), Some(y)) => x.path.cmp(&y.path),
        };
        let (path, ranges) = match cmp {
            std::cmp::Ordering::Less => {
                i += 1;
                (a[i - 1].path.clone(), a[i - 1].ranges.clone())
            }
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
                (
                    a[i - 1].path.clone(),
                    RangeSet::union(&a[i - 1].ranges, &b[j - 1].ranges),
                )
            }
            std::cmp::Ordering::Greater => {
                j += 1;
                (b[j - 1].path.clone(), b[j - 1].ranges.clone())
            }
        };
        out.push(FileRanges { path, ranges, pair: None });
    }
    out
}

// ---------------------------------------------------------------------------
// argument parsing (line-range.c)
// ---------------------------------------------------------------------------

/// `fill_line_ends`: the offset of every line terminator, with a leading `0` so that
/// `ends.len() - 1` is the line count and `nth_line` can index it directly.
fn fill_line_ends(data: &[u8]) -> Vec<usize> {
    let mut ends = vec![0usize];
    let size = data.len();
    let mut num = 0usize;
    while num < size {
        if data[num] == b'\n' || num == size - 1 {
            ends.push(num);
        }
        num += 1;
    }
    ends
}

/// `nth_line`: the byte offset at which the 0-based line `line` begins. git returns
/// a pointer into the NUL-terminated blob, so everything downstream works on the
/// suffix starting here.
fn nth_line(ends: &[usize], line: i64) -> usize {
    if line <= 0 {
        0
    } else {
        ends[line as usize] + 1
    }
}

/// The blob a `-L` spec is resolved against, plus its line index.
struct Blob<'a> {
    data: &'a [u8],
    ends: Vec<usize>,
}

impl Blob<'_> {
    fn at(&self, line: i64) -> usize {
        nth_line(&self.ends, line)
    }
}

/// `strtol(s, &end, 10)` over ASCII: the value and how many bytes it consumed.
/// Zero consumed bytes is C's "no conversion performed".
fn strtol(s: &str) -> (i64, usize) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let neg = matches!(b.get(i), Some(b'-'));
    if matches!(b.get(i), Some(b'+') | Some(b'-')) {
        i += 1;
    }
    let digits = i;
    let mut val: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        val = val.saturating_mul(10).saturating_add((b[i] - b'0') as i64);
        i += 1;
    }
    if i == digits {
        return (0, 0);
    }
    (if neg { -val } else { val }, i)
}

/// Compile one `-L` regular expression. git calls `regcomp(pattern, REG_NEWLINE)`,
/// so the dialect is POSIX *basic* (no `REG_EXTENDED`) and `^`/`$` anchor at line
/// boundaries while `.` does not cross one.
fn compile(pattern: &str) -> std::result::Result<regex::bytes::Regex, String> {
    if let Some(msg) = bre_syntax_error(pattern) {
        return Err(msg.to_string());
    }
    regex::bytes::RegexBuilder::new(&crate::revfilter::bre_to_regex(pattern))
        .multi_line(true)
        .dot_matches_new_line(false)
        .unicode(false)
        .build()
        .map_err(|_| "invalid regular expression".to_string())
}

/// The POSIX `regcomp` diagnostics this port reproduces verbatim. Checked before
/// handing the pattern to the regex crate, whose own error text is its own.
///
/// In an extended regular expression the grouping and interval operators are the
/// *bare* `(`/`)` and `{`/`}` — the escaped forms are literals — which is the one
/// difference between the two dialects here. `-L`'s own patterns are BRE, hence
/// the default.
pub(crate) fn bre_syntax_error(pattern: &str) -> Option<&'static str> {
    syntax_error(pattern, false)
}

/// [`bre_syntax_error`] for an extended regular expression.
pub(crate) fn ere_syntax_error(pattern: &str) -> Option<&'static str> {
    syntax_error(pattern, true)
}

fn syntax_error(pattern: &str, extended: bool) -> Option<&'static str> {
    let b = pattern.as_bytes();
    let (mut parens, mut braces) = (0i32, 0i32);
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'[' => {
                // A bracket expression: `]` first is a literal, and `[:...:]`,
                // `[....]` and `[==]` nest one level.
                let mut j = i + 1;
                if matches!(b.get(j), Some(b'^')) {
                    j += 1;
                }
                if matches!(b.get(j), Some(b']')) {
                    j += 1;
                }
                let mut closed = false;
                while j < b.len() {
                    if b[j] == b'[' && matches!(b.get(j + 1), Some(b':' | b'.' | b'=')) {
                        let kind = b[j + 1];
                        let mut k = j + 2;
                        while k + 1 < b.len() && !(b[k] == kind && b[k + 1] == b']') {
                            k += 1;
                        }
                        if k + 1 >= b.len() {
                            return Some("brackets ([ ]) not balanced");
                        }
                        j = k + 2;
                        continue;
                    }
                    if b[j] == b']' {
                        closed = true;
                        break;
                    }
                    j += 1;
                }
                if !closed {
                    return Some("brackets ([ ]) not balanced");
                }
                i = j + 1;
                continue;
            }
            // In an ERE these four are the operators themselves; in a BRE they
            // are literals and the escaped forms below are the operators.
            b'(' | b')' | b'{' | b'}' if extended => {
                match b[i] {
                    b'(' => parens += 1,
                    b')' => {
                        parens -= 1;
                        if parens < 0 {
                            return Some("parentheses not balanced");
                        }
                    }
                    b'{' => braces += 1,
                    _ => {
                        braces -= 1;
                        if braces < 0 {
                            return Some("braces not balanced");
                        }
                    }
                }
                i += 1;
                continue;
            }
            b'\\' => {
                let Some(&n) = b.get(i + 1) else {
                    return Some("trailing backslash (\\)");
                };
                if extended {
                    // An escaped operator is a literal here, and a `\1` back
                    // reference is not an ERE construct at all.
                    i += 2;
                    continue;
                }
                match n {
                    b'(' => parens += 1,
                    b')' => {
                        parens -= 1;
                        if parens < 0 {
                            return Some("parentheses not balanced");
                        }
                    }
                    b'{' => {
                        braces += 1;
                        // `\{n,m\}` with m < n is git's "invalid repetition count(s)".
                        let rest = &pattern[i + 2..];
                        let (lo, used) = strtol(rest);
                        if used > 0 && rest.as_bytes().get(used) == Some(&b',') {
                            let (hi, used2) = strtol(&rest[used + 1..]);
                            if used2 > 0 && hi < lo {
                                return Some("invalid repetition count(s)");
                            }
                        }
                    }
                    b'}' => {
                        braces -= 1;
                        if braces < 0 {
                            return Some("braces not balanced");
                        }
                    }
                    b'1'..=b'9' => {
                        if (n - b'0') as i32 > parens {
                            return Some("invalid backreference number");
                        }
                    }
                    _ => {}
                }
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    if parens != 0 {
        return Some("parentheses not balanced");
    }
    if braces != 0 {
        return Some("braces not balanced");
    }
    None
}

/// `parse_loc`: one endpoint of a `-L<start>,<end>` spec.
///
/// `begin` carries git's two-way convention: negative while parsing the *start*
/// (its absolute value anchors relative forms, `-1` meaning start of file), and the
/// positive line after the resolved start while parsing the *end*. `out` is `None`
/// for `skip_range_arg`'s scan-only pass, which must consume exactly the same bytes
/// without touching the blob.
fn parse_loc<'a>(
    spec: &'a str,
    blob: Option<&Blob<'_>>,
    lines: i64,
    mut begin: i64,
    out: Option<&mut i64>,
) -> R<&'a str> {
    let mut spec = spec;
    // "-L <something>,+20" is 20 lines from <something>; ",-5" ends there.
    if begin >= 1 && (spec.starts_with('+') || spec.starts_with('-')) {
        let sign_minus = spec.starts_with('-');
        let (mut num, used) = strtol(&spec[1..]);
        if used != 0 {
            let term = &spec[1 + used..];
            let Some(out) = out else { return Ok(term) };
            if num == 0 {
                return die("-L invalid empty range");
            }
            if sign_minus {
                num = -num;
            }
            *out = if num > 0 {
                begin + num - 2
            } else if begin + num > 0 {
                begin + num
            } else {
                1
            };
            return Ok(term);
        }
        return Ok(spec);
    }

    let (num, used) = strtol(spec);
    if used != 0 {
        if let Some(out) = out {
            if num <= 0 {
                return die(format!("-L invalid line number: {num}"));
            }
            *out = num;
        }
        return Ok(&spec[used..]);
    }

    if begin < 0 {
        if !spec.starts_with('^') {
            begin = -begin;
        } else {
            begin = 1;
            spec = &spec[1..];
        }
    }

    if !spec.starts_with('/') {
        return Ok(spec);
    }

    // A `/.../` regexp: the closing slash may be backslash-escaped.
    let b = spec.as_bytes();
    let mut t = 1usize;
    while t < b.len() && b[t] != b'/' {
        if b[t] == b'\\' {
            t += 1;
        }
        t += 1;
    }
    if t >= b.len() || b[t] != b'/' {
        return Ok(spec);
    }
    let Some(out) = out else { return Ok(&spec[t + 1..]) };

    let pattern = &spec[1..t];
    let blob = blob.expect("resolving a -L regexp needs the blob");
    begin -= 1; // input is in human terms
    let mut line_off = blob.at(begin);

    let re = match compile(pattern) {
        Ok(re) => re,
        Err(msg) => {
            return die(format!(
                "-L parameter '{pattern}' starting at line {}: {msg}",
                begin + 1
            ))
        }
    };
    let Some(m) = re.find(&blob.data[line_off..]) else {
        return die(format!(
            "-L parameter '{pattern}' starting at line {}: regexec() failed to match",
            begin + 1
        ));
    };
    let cp = line_off + m.start();
    loop {
        let more = begin < lines;
        begin += 1;
        if !more {
            break;
        }
        let nline = blob.at(begin);
        if line_off <= cp && cp < nline {
            break;
        }
        line_off = nline;
    }
    *out = begin;
    Ok(&spec[t + 1..])
}

/// `match_funcname` with no `diff=<driver>` pattern: xdiff's built-in heading test.
fn match_funcname(
    ff: Option<&crate::userdiff::FuncName>,
    data: &[u8],
    bol: usize,
    eol: usize,
) -> bool {
    match ff {
        // `xecfg->find_func(bol, eol - bol, buf, 1, priv) >= 0` — only whether the
        // pattern matched is read, which is why git hands it a one-byte buffer.
        Some(f) => f.find(&data[bol..eol], 1).is_some(),
        None => bol != eol && def_ff(&data[bol..eol], 1).is_some(),
    }
}

/// `find_funcname_matching_regexp`: the first match of `re` at or after `start` that
/// sits on a line reading as a function heading.
fn find_funcname_matching_regexp(
    ff: Option<&crate::userdiff::FuncName>,
    data: &[u8],
    start: usize,
    re: &regex::bytes::Regex,
) -> Option<usize> {
    let mut start = start;
    while start < data.len() {
        let m = re.find(&data[start..])?;
        let (ms, me) = (start + m.start(), start + m.end());
        // Widen the match to the line that holds it.
        let mut bol = ms;
        while bol > start {
            bol -= 1;
            if data[bol] == b'\n' {
                break;
            }
        }
        if data.get(bol) == Some(&b'\n') {
            bol += 1;
        }
        let mut eol = me;
        while eol < data.len() && data[eol] != b'\n' {
            eol += 1;
        }
        if data.get(eol) == Some(&b'\n') {
            eol += 1;
        }
        if match_funcname(ff, data, bol, eol) {
            return Some(bol);
        }
        if eol <= start {
            return None;
        }
        start = eol;
    }
    None
}

/// `parse_range_funcname`: `:<funcname>` and `^:<funcname>`, which expand to the
/// span from the matching heading line to the next one.
fn parse_range_funcname<'a>(
    arg: &'a str,
    blob: Option<&Blob<'_>>,
    lines: i64,
    mut anchor: i64,
    out: Option<(&mut i64, &mut i64)>,
    // `xecfg->find_func`: the diff driver the `-L` path's `diff` gitattribute names,
    // if it carries a funcname pattern.
    ff: Option<&crate::userdiff::FuncName>,
) -> R<Option<&'a str>> {
    let mut arg = arg;
    if arg.starts_with('^') {
        anchor = 1;
        arg = &arg[1..];
    }
    debug_assert!(arg.starts_with(':'));

    let b = arg.as_bytes();
    let mut t = 1usize;
    while t < b.len() && b[t] != b':' {
        if b[t] == b'\\' && t + 1 < b.len() {
            t += 1;
        }
        t += 1;
    }
    if t == 1 {
        return Ok(None);
    }
    let Some((begin, end)) = out else { return Ok(Some(&arg[t..])) };

    let pattern = &arg[1..t];
    let blob = blob.expect("resolving a -L funcname needs the blob");
    anchor -= 1; // input is in human terms
    let start = blob.at(anchor);

    let re = match compile(pattern) {
        Ok(re) => re,
        Err(msg) => return die(format!("-L parameter '{pattern}': {msg}")),
    };
    let Some(p) = find_funcname_matching_regexp(ff, blob.data, start, &re) else {
        return die(format!(
            "-L parameter '{pattern}' starting at line {}: no match",
            anchor + 1
        ));
    };

    *begin = 0;
    while p > blob.at(*begin) {
        *begin += 1;
    }
    if *begin >= lines {
        return die(format!("-L parameter '{pattern}' matches at EOF"));
    }
    *end = *begin + 1;
    while *end < lines {
        let bol = blob.at(*end);
        let eol = blob.at(*end + 1);
        if match_funcname(ff, blob.data, bol, eol) {
            break;
        }
        *end += 1;
    }
    *begin += 1; // compensate for 1-based numbering
    Ok(Some(&arg[t..]))
}

/// `parse_range_arg`: resolve one range spec into a 1-based inclusive `(begin, end)`
/// against `blob`. `Ok(None)` is git's `-1` return, i.e. "malformed".
fn parse_range_arg(
    arg: &str,
    blob: &Blob<'_>,
    lines: i64,
    mut anchor: i64,
    ff: Option<&crate::userdiff::FuncName>,
) -> R<Option<(i64, i64)>> {
    let (mut begin, mut end) = (0i64, 0i64);
    if anchor < 1 {
        anchor = 1;
    }
    if anchor > lines {
        anchor = lines + 1;
    }

    if arg.starts_with(':') || (arg.starts_with("^:")) {
        let rest =
            parse_range_funcname(arg, Some(blob), lines, anchor, Some((&mut begin, &mut end)), ff)?;
        return match rest {
            Some(r) if r.is_empty() => Ok(Some((begin, end))),
            _ => Ok(None),
        };
    }

    let rest = parse_loc(arg, Some(blob), lines, -anchor, Some(&mut begin))?;
    let rest = if let Some(after) = rest.strip_prefix(',') {
        parse_loc(after, Some(blob), lines, begin + 1, Some(&mut end))?
    } else {
        rest
    };
    if !rest.is_empty() {
        return Ok(None);
    }
    if begin != 0 && end != 0 && end < begin {
        std::mem::swap(&mut begin, &mut end);
    }
    Ok(Some((begin, end)))
}

/// `skip_range_arg`: consume the range part of a `-L` argument without resolving it,
/// leaving the `:<file>` tail. `Ok(None)` is git's NULL return.
fn skip_range_arg(arg: &str) -> R<Option<&str>> {
    if arg.starts_with(':') || arg.starts_with("^:") {
        // Skipping consults no funcname pattern: `skip_range_arg()` never resolves.
        return parse_range_funcname(arg, None, 0, 0, None, None);
    }
    let rest = parse_loc(arg, None, 0, -1, None)?;
    let rest = match rest.strip_prefix(',') {
        Some(after) => parse_loc(after, None, 0, 0, None)?,
        None => rest,
    };
    Ok(Some(rest))
}

// ---------------------------------------------------------------------------
// initialization (line-log.c's `parse_lines`)
// ---------------------------------------------------------------------------

/// Resolve a `-L` path the way git's `prefix_path` does: relative to the directory
/// the command was run in, normalized against the work tree root.
fn prefix_path(repo: &gix::Repository, name: &str) -> BString {
    let prefix = (|| {
        let workdir = repo.workdir()?.canonicalize().ok()?;
        let cwd = std::env::current_dir().ok()?.canonicalize().ok()?;
        let rel = cwd.strip_prefix(&workdir).ok()?;
        let s = gix::path::into_bstr(rel).into_owned();
        (!s.is_empty()).then_some(s)
    })();

    let mut parts: Vec<&[u8]> = Vec::new();
    let joined: BString = match &prefix {
        Some(p) if !name.starts_with('/') => {
            let mut v = p.clone();
            v.push(b'/');
            v.extend_from_slice(name.as_bytes());
            v
        }
        _ => BString::from(name.as_bytes()),
    };
    for seg in joined.split(|b| *b == b'/') {
        match seg {
            b"" | b"." => {}
            b".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let mut out = BString::from(parts.join(&b'/'));
    // `normalize_path_copy()` collapses `.`, `..` and repeated slashes but keeps a
    // trailing one, so `-L1,2:a.c/` still asks the tree for `a.c/` — and does not
    // find it.
    if joined.last() == Some(&b'/') && !out.is_empty() {
        out.push(b'/');
    }
    out
}

/// `parse_lines`: turn the raw `-L` arguments into the per-path range list the walk
/// starts from, resolved against `commit`'s trees.
pub(crate) fn parse_lines(
    repo: &gix::Repository,
    commit: ObjectId,
    args: &[String],
) -> R<Tracked> {
    let mut ranges: Tracked = Vec::new();
    for item in args {
        let Some(name_part) = skip_range_arg(item)? else {
            return die(format!(
                "-L argument not 'start,end:file' or ':funcname:file': {item}"
            ));
        };
        if !name_part.starts_with(':') || name_part.len() < 2 {
            return die(format!(
                "-L argument not 'start,end:file' or ':funcname:file': {item}"
            ));
        }
        let range_part = &item[..item.len() - name_part.len()];
        let name_part = &name_part[1..];
        let full_name = prefix_path(repo, name_part);

        let data = blob_at(repo, commit, &full_name)?;
        let ends = fill_line_ends(&data);
        let lines = ends.len() as i64 - 1;
        let blob = Blob { data: &data, ends };

        let anchor = match ranges.binary_search_by(|p| p.path.cmp(&full_name)) {
            Ok(i) if !ranges[i].ranges.is_empty() => {
                ranges[i].ranges.ranges.last().expect("non-empty").end + 1
            }
            _ => 1,
        };

        // `parse_range_funcname()` (line-range.c:152) resolves the path's diff
        // driver and installs its funcname pattern, so `:<re>` searches that
        // driver's section headings rather than xdiff's built-in heuristic.
        let ff = match super::blame::line_range_funcname(repo, &String::from_utf8_lossy(&full_name)) {
            Ok(f) => f,
            Err(msg) => return die(msg),
        };
        let Some((mut begin, mut end)) =
            parse_range_arg(range_part, &blob, lines, anchor, ff.as_ref().and_then(|d| d.funcname.as_ref()))?
        else {
            return die(format!("malformed -L argument '{range_part}'"));
        };
        if (lines == 0 && (begin != 0 || end != 0)) || lines < begin {
            return die(format!("file {name_part} has only {lines} lines"));
        }
        if begin < 1 {
            begin = 1;
        }
        if end < 1 || lines < end {
            end = lines;
        }
        begin -= 1;
        insert(&mut ranges, full_name, begin, end);
    }
    for p in &mut ranges {
        p.ranges.sort_and_merge();
    }
    Ok(ranges)
}

/// `fill_blob_sha1` + `diff_populate_filespec`: the content of `path` in `commit`.
fn blob_at(repo: &gix::Repository, commit: ObjectId, path: &BString) -> R<Vec<u8>> {
    match tree_entry(repo, commit, path) {
        Ok(Some((id, _))) => match repo.find_object(id) {
            Ok(obj) => Ok(obj.data.clone()),
            Err(_) => die(format!("Cannot read blob {id}")),
        },
        _ => die(format!("There is no path {path} in the commit")),
    }
}

/// The blob id and mode of `path` in `commit`'s tree, or `None` when absent.
fn tree_entry(
    repo: &gix::Repository,
    commit: ObjectId,
    path: &BString,
) -> anyhow::Result<Option<(ObjectId, EntryKind)>> {
    let tree = repo.find_object(commit)?.try_into_commit()?.tree()?;
    Ok(tree
        .lookup_entry(path.split(|b| *b == b'/'))?
        .map(|e| (e.object_id(), e.mode().kind())))
}

// ---------------------------------------------------------------------------
// traversal (line-log.c)
// ---------------------------------------------------------------------------

/// The per-commit range bookkeeping of a `-L` walk: git's `revs->line_log_data`
/// decoration, carried from a commit to its parents as the walk proceeds.
pub(crate) struct Tracker<'a> {
    repo: &'a gix::Repository,
    state: HashMap<ObjectId, Tracked>,
    /// `rev->first_parent_only`: a merge is only ever asked of its first parent.
    first_parent: bool,
    /// Blob cache: the walk reads the same object as "target" at one commit and
    /// "parent" at the next.
    blobs: HashMap<ObjectId, std::rc::Rc<Vec<u8>>>,
    /// Tree lookups, memoised for the same reason: each commit is probed once as the
    /// target and again as its child's parent, for every tracked path.
    entries: HashMap<(ObjectId, BString), Side>,
}

impl<'a> Tracker<'a> {
    pub(crate) fn new(repo: &'a gix::Repository, start: ObjectId, ranges: Tracked, first_parent: bool) -> Self {
        let mut state = HashMap::new();
        state.insert(start, ranges);
        Tracker { repo, state, first_parent, blobs: HashMap::new(), entries: HashMap::new() }
    }

    /// [`tree_entry`] against the memo: the walk asks for the same commit and path
    /// twice, once as the target and once as its child's parent.
    fn entry(&mut self, commit: ObjectId, path: &BString) -> anyhow::Result<Side> {
        let key = (commit, path.clone());
        if let Some(side) = self.entries.get(&key) {
            return Ok(*side);
        }
        let side = tree_entry(self.repo, commit, path)?;
        self.entries.insert(key, side);
        Ok(side)
    }

    fn blob(&mut self, id: ObjectId) -> anyhow::Result<std::rc::Rc<Vec<u8>>> {
        if let Some(b) = self.blobs.get(&id) {
            return Ok(b.clone());
        }
        let data = std::rc::Rc::new(self.repo.find_object(id)?.data.clone());
        self.blobs.insert(id, data.clone());
        Ok(data)
    }

    /// `line_log_process_ranges_arbitrary_commit`: map this commit's tracked ranges
    /// back onto its parents.
    ///
    /// The first result is the commit's own record when it took blame for a tracked
    /// range (git's `changed`), and `None` when it is TREESAME and dropped. The
    /// second is its parent list, which a merge whose blame one parent could absorb
    /// has had rewritten down to that parent.
    pub(crate) fn process(
        &mut self,
        commit: ObjectId,
        parents: &[ObjectId],
    ) -> anyhow::Result<(Option<Tracked>, Vec<ObjectId>)> {
        let Some(range) = self.state.remove(&commit) else {
            return Ok((None, parents.to_vec()));
        };
        let nparents = if self.first_parent { parents.len().min(1) } else { parents.len() };
        if parents.len() > 1 {
            self.merge_commit(commit, &parents[..nparents], range)
        } else {
            let kept = self.ordinary_commit(commit, parents.first().copied(), range)?;
            Ok((kept, parents.to_vec()))
        }
    }

    /// `process_ranges_ordinary_commit`.
    fn ordinary_commit(
        &mut self,
        commit: ObjectId,
        parent: Option<ObjectId>,
        mut range: Tracked,
    ) -> anyhow::Result<Option<Tracked>> {
        let queue = self.queue_diffs(commit, parent, &range)?;
        let (parent_range, changed) = self.process_all_files(&queue, &mut range)?;
        if let Some(p) = parent {
            self.add_line_range(p, parent_range);
        }
        Ok(changed.then_some(range))
    }

    /// `process_ranges_merge_commit`: ask each parent in turn whether it can take all
    /// the blame; the first that can ends the search, and the merge is dropped with
    /// its parent list rewritten down to that one parent.
    ///
    /// When no parent can, the merge is kept — but `clear_commit_line_range()` still
    /// drops its own record, which is why a `-L` merge never shows a diff.
    fn merge_commit(
        &mut self,
        commit: ObjectId,
        parents: &[ObjectId],
        mut range: Tracked,
    ) -> anyhow::Result<(Option<Tracked>, Vec<ObjectId>)> {
        let mut cand: Vec<Tracked> = Vec::with_capacity(parents.len());
        for parent in parents {
            let queue = self.queue_diffs(commit, Some(*parent), &range)?;
            // Each pass records its pairs on `range` itself, trampling the previous
            // parent's, exactly as git's NEEDSWORK comment describes.
            let (parent_range, changed) = self.process_all_files(&queue, &mut range)?;
            if !changed {
                // This parent takes all the blame, so no other path is followed.
                self.add_line_range(*parent, parent_range);
                return Ok((None, vec![*parent]));
            }
            cand.push(parent_range);
        }
        for (parent, c) in parents.iter().zip(cand) {
            self.add_line_range(*parent, c);
        }
        Ok((Some(Vec::new()), parents.to_vec()))
    }

    /// `add_line_range`: hand a range list to a parent, merging with whatever another
    /// child already left there.
    fn add_line_range(&mut self, commit: ObjectId, range: Tracked) {
        match self.state.get(&commit) {
            Some(old) => {
                let merged = merge(old, &range);
                self.state.insert(commit, merged);
            }
            None => {
                self.state.insert(commit, range);
            }
        }
    }

    /// `queue_diffs`: the file pairs of `commit` against `parent`, limited to the
    /// tracked paths (git builds that pathspec out of the range list itself).
    fn queue_diffs(
        &mut self,
        commit: ObjectId,
        parent: Option<ObjectId>,
        range: &Tracked,
    ) -> anyhow::Result<Vec<Pair>> {
        let mut out = Vec::new();
        for rg in range {
            let new = self.entry(commit, &rg.path)?;
            let old = match parent {
                Some(p) => self.entry(p, &rg.path)?,
                None => None,
            };
            if old == new {
                continue;
            }
            out.push(Pair::same_path(rg.path.clone(), old, new));
        }
        // `diff_might_be_rename()`: a tracked path that appears out of nowhere may
        // really be a `git mv`, and only the full tree diff can say so.
        if out.iter().any(|p| p.old.is_none() && p.new.is_some()) {
            if let Some(parent) = parent {
                out = self.rename_pass(commit, parent, range)?;
            }
        }
        Ok(out)
    }

    /// `queue_diffs`' rename branch: rerun the diff over the whole tree, keep the
    /// tracked destinations plus every deletion (a rename source is a deletion until
    /// detection pairs it up), run `diffcore_std`'s rename pass, then keep only the
    /// tracked destinations.
    fn rename_pass(
        &mut self,
        commit: ObjectId,
        parent: ObjectId,
        range: &Tracked,
    ) -> anyhow::Result<Vec<Pair>> {
        let new_tree = self.repo.find_object(commit)?.try_into_commit()?.tree()?;
        let old_tree = self.repo.find_object(parent)?.try_into_commit()?.tree()?;
        let changes = self.repo.diff_tree_to_tree(
            Some(&old_tree),
            Some(&new_tree),
            gix::diff::Options::default(),
        )?;

        let tracked = |path: &BString| range.iter().any(|r| &r.path == path);
        let mut q = diffcore_rename::Queue::default();
        for change in &changes {
            let Some((path, old, new)) = tree_change_sides(change) else {
                continue;
            };
            // `filter_diffs_for_paths(range, keep_deletions = 1)`.
            if new.is_some() && !tracked(&path) {
                continue;
            }
            let one = match old {
                Some((id, k)) => diffcore_rename::FileSpec::new(path.clone(), kind_mode(k), id, true),
                None => diffcore_rename::FileSpec::absent(path.clone()),
            };
            let two = match new {
                Some((id, k)) => diffcore_rename::FileSpec::new(path.clone(), kind_mode(k), id, true),
                None => diffcore_rename::FileSpec::absent(path.clone()),
            };
            let (one, two) = (q.add_spec(one), q.add_spec(two));
            q.add_pair(one, two);
        }

        // git builds a private `diff_options` for this pass so the caller's own
        // knobs cannot discard pairs rename tracking needs; `git log` is a porcelain,
        // so `diff.renames` leaves plain rename detection on at the default score.
        let opts = diffcore_rename::Options {
            detect_rename: diffcore_rename::DETECT_RENAME,
            hash_kind: self.repo.object_hash(),
            ..Default::default()
        };
        let mut content = BlobContent { repo: self.repo };
        diffcore_rename::run(&mut q, &opts, &mut content);
        diffcore_rename::resolve_rename_copy(&mut q);

        let mut out = Vec::new();
        for p in &q.pairs {
            let (one, two) = (&q.specs[p.one], &q.specs[p.two]);
            // `filter_diffs_for_paths(range, keep_deletions = 0)`.
            if !two.valid() || !tracked(&two.path) {
                continue;
            }
            out.push(Pair {
                path: two.path.clone(),
                old_path: if one.valid() { one.path.clone() } else { two.path.clone() },
                old: one.valid().then(|| (one.oid, mode_kind(one.mode))),
                new: Some((two.oid, mode_kind(two.mode))),
            });
        }
        Ok(out)
    }

    /// `process_all_files`: map every queued pair across the diff, recording on the
    /// *input* record the pair that took the blame.
    fn process_all_files(
        &mut self,
        queue: &[Pair],
        range: &mut Tracked,
    ) -> anyhow::Result<(Tracked, bool)> {
        let mut range_out = range.clone();
        let mut changed = false;
        for pair in queue {
            if self.process_diff_filepair(pair, &mut range_out)? {
                changed = true;
                if let Some(rg) = range.iter_mut().find(|r| r.path == pair.path) {
                    rg.pair = Some(pair.clone());
                }
            }
        }
        for r in &mut range_out {
            r.pair = None;
        }
        // A rename rewrote a record's path, which git leaves where it was; keeping the
        // list sorted preserves the invariant `merge()` and `insert()` rely on.
        range_out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok((range_out, changed))
    }

    /// `process_diff_filepair`: destructively map one file's ranges back through its
    /// diff, reporting whether any hunk was touched. The record's path becomes the
    /// pre-image path, which is what carries a range across a rename.
    fn process_diff_filepair(&mut self, pair: &Pair, range: &mut Tracked) -> anyhow::Result<bool> {
        let Some(rg) = range.iter_mut().find(|r| r.path == pair.path) else {
            return Ok(false);
        };
        if rg.ranges.is_empty() {
            return Ok(false);
        }
        // A pair whose new side is gone cannot be mapped: git asserts on
        // `two->oid_valid`, which only a deleted path could violate, and a deletion
        // can never be reached with live ranges (the re-creation stops them first).
        let Some((new_id, _)) = pair.new else { return Ok(false) };
        let target = self.blob(new_id)?;
        let parent = match pair.old {
            Some((id, _)) => self.blob(id)?,
            None => std::rc::Rc::new(Vec::new()),
        };

        let diff = collect_diff(&parent, &target);
        // The record now belongs to the pre-image path, which is how a tracked range
        // follows its file across a rename.
        rg.path = pair.old_path.clone();
        let (mapped, touched) = RangeSet::map_across_diff(&rg.ranges, &diff);
        rg.ranges = mapped;
        Ok(!touched.parent.ranges.is_empty())
    }
}

/// `diff_populate_filespec` for the rename pass: both sides are always object-backed
/// here, so size comes from the object header and content from the odb.
struct BlobContent<'a> {
    repo: &'a gix::Repository,
}

impl diffcore_rename::Content for BlobContent<'_> {
    fn size(&mut self, spec: &diffcore_rename::FileSpec) -> Option<u64> {
        let header = self.repo.find_header(spec.oid).ok()?;
        (header.kind() == gix::object::Kind::Blob).then(|| header.size())
    }

    fn data(&mut self, spec: &diffcore_rename::FileSpec) -> Option<Vec<u8>> {
        Some(self.repo.find_object(spec.oid).ok()?.data.clone())
    }
}

/// The two sides of one tree-diff entry, or `None` for the directory entries gix
/// reports alongside the files it recurses into.
fn tree_change_sides(
    change: &gix::object::tree::diff::ChangeDetached,
) -> Option<(BString, Side, Side)> {
    use gix::object::tree::diff::ChangeDetached as C;
    match change {
        C::Addition { location, entry_mode, id, .. } => {
            let kind = entry_mode.kind();
            (kind != EntryKind::Tree).then(|| (location.clone(), None, Some((*id, kind))))
        }
        C::Deletion { location, entry_mode, id, .. } => {
            let kind = entry_mode.kind();
            (kind != EntryKind::Tree).then(|| (location.clone(), Some((*id, kind)), None))
        }
        C::Modification { location, previous_entry_mode, previous_id, entry_mode, id, .. } => {
            let (pk, nk) = (previous_entry_mode.kind(), entry_mode.kind());
            (pk != EntryKind::Tree && nk != EntryKind::Tree)
                .then(|| (location.clone(), Some((*previous_id, pk)), Some((*id, nk))))
        }
        C::Rewrite { .. } => None,
    }
}

/// `EntryKind` as the full octal mode git's filespecs carry.
///
/// `pub(crate)` because `log`'s `-L --raw` writer needs the same mode bytes
/// `diff_flush_raw()` prints.
pub(crate) fn kind_mode(kind: EntryKind) -> u32 {
    u32::from(gix::objs::tree::EntryMode::from(kind).value())
}

/// The inverse, for turning a rename-pass filespec back into a side.
fn mode_kind(mode: u32) -> EntryKind {
    match mode & 0o170000 {
        0o120000 => EntryKind::Link,
        0o160000 => EntryKind::Commit,
        0o040000 => EntryKind::Tree,
        _ if mode & 0o111 != 0 => EntryKind::BlobExecutable,
        _ => EntryKind::Blob,
    }
}

// ---------------------------------------------------------------------------
// output (diff.c's `line_range_callback`)
// ---------------------------------------------------------------------------

/// `line_log_queue_pairs`: the file pairs of a shown commit, in path order, each
/// with the ranges the output must be clipped to.
pub(crate) fn queue_pairs(range: &Tracked) -> Vec<(Pair, Vec<Range>)> {
    range
        .iter()
        .filter_map(|r| r.pair.clone().map(|p| (p, r.ranges.ranges.clone())))
        .collect()
}

/// The wrappers that sit between xdiff and `fn_out_consume` under `-L`
/// (`struct line_range_callback`): xdiff produces a normal unified diff, and these
/// forward only the lines that fall inside a tracked range, re-headering each
/// contiguous run as its own hunk.
///
/// Removal lines cannot be placed by post-image position, so they are held in
/// `pending_rm` until the next `+`/` ` line says whether they precede in-range
/// content (flush) or out-of-range content (discard).
pub(crate) struct RangeSink<'a> {
    buf: Vec<u8>,
    before: &'a [&'a [u8]],
    after: &'a [&'a [u8]],
    ranges: &'a [Range],
    cur_range: usize,

    /// git's `funclineprev`/`func_line`, as in the unfiltered patch sink: the
    /// heading search is bounded by the previous hunk and its answer persists.
    func_prev: i64,
    func_text: Vec<u8>,
    /// The heading of the most recent xdiff hunk, which is what a flushed range
    /// hunk is labelled with.
    func: Vec<u8>,

    lno_post: i64,
    lno_pre: i64,

    rhunk: Vec<u8>,
    rhunk_old_begin: i64,
    rhunk_old_count: i64,
    rhunk_new_begin: i64,
    rhunk_new_count: i64,
    rhunk_active: bool,
    rhunk_has_changes: bool,

    pending_rm: Vec<u8>,
    pending_rm_count: i64,
    pending_rm_pre_begin: i64,
}

impl<'a> RangeSink<'a> {
    pub(crate) fn new(
        before: &'a [&'a [u8]],
        after: &'a [&'a [u8]],
        ranges: &'a [Range],
    ) -> Self {
        RangeSink {
            buf: Vec::new(),
            before,
            after,
            ranges,
            cur_range: 0,
            func_prev: -1,
            func_text: Vec::new(),
            func: Vec::new(),
            lno_post: 0,
            lno_pre: 0,
            rhunk: Vec::new(),
            rhunk_old_begin: 0,
            rhunk_old_count: 0,
            rhunk_new_begin: 0,
            rhunk_new_count: 0,
            rhunk_active: false,
            rhunk_has_changes: false,
            pending_rm: Vec::new(),
            pending_rm_count: 0,
            pending_rm_pre_begin: 0,
        }
    }

    /// The context xdiff must run with so that every change inside one range lands in
    /// a single hunk: git inflates `ctxlen` to the widest tracked span.
    pub(crate) fn context(ranges: &[Range], ctx: u32) -> u32 {
        let max_span = ranges.iter().map(|r| r.end - r.start).max().unwrap_or(0);
        if max_span > ctx as i64 {
            max_span.min(u32::MAX as i64) as u32
        } else {
            ctx
        }
    }

    fn discard_pending_rm(&mut self) {
        self.pending_rm.clear();
        self.pending_rm_count = 0;
    }

    /// `flush_rhunk`: emit the accumulated range hunk under a synthetic `@@` header.
    fn flush_rhunk(&mut self) {
        if !self.rhunk_active {
            return;
        }
        if self.pending_rm_count != 0 {
            self.rhunk.extend_from_slice(&self.pending_rm);
            self.rhunk_old_count += self.pending_rm_count;
            self.rhunk_has_changes = true;
            self.discard_pending_rm();
        }
        // A context-only fragment carries no change and would be noise; the inflated
        // context can produce one for a range this commit did not touch.
        if !self.rhunk_has_changes {
            self.rhunk_active = false;
            self.rhunk.clear();
            return;
        }
        self.buf.extend_from_slice(
            format!(
                "@@ -{},{} +{},{} @@",
                self.rhunk_old_begin,
                self.rhunk_old_count,
                self.rhunk_new_begin,
                self.rhunk_new_count
            )
            .as_bytes(),
        );
        if !self.func.is_empty() {
            self.buf.push(b' ');
            self.buf.extend_from_slice(&self.func);
        }
        self.buf.push(b'\n');
        let rhunk = std::mem::take(&mut self.rhunk);
        self.buf.extend_from_slice(&rhunk);
        self.rhunk_active = false;
    }

    /// `line_range_line_fn`: one already-prefixed diff line.
    fn line(&mut self, line: &[u8]) {
        match line.first() {
            Some(b'-') => {
                if self.pending_rm_count == 0 {
                    self.pending_rm_pre_begin = self.lno_pre;
                }
                self.lno_pre += 1;
                self.pending_rm.extend_from_slice(line);
                self.pending_rm_count += 1;
                return;
            }
            Some(b'\\') => {
                if self.pending_rm_count != 0 {
                    self.pending_rm.extend_from_slice(line);
                } else if self.rhunk_active {
                    self.rhunk.extend_from_slice(line);
                }
                return;
            }
            _ => {}
        }

        let lno_0 = self.lno_post - 1;
        let cur_pre = self.lno_pre; // saved before the context advance
        self.lno_post += 1;
        if line.first() == Some(&b' ') {
            self.lno_pre += 1;
        }

        while self.cur_range < self.ranges.len() && lno_0 >= self.ranges[self.cur_range].end {
            if self.rhunk_active {
                self.flush_rhunk();
            }
            self.discard_pending_rm();
            self.cur_range += 1;
        }
        if self.cur_range >= self.ranges.len() {
            self.discard_pending_rm();
            return;
        }
        let cur = self.ranges[self.cur_range];
        if lno_0 < cur.start {
            self.discard_pending_rm();
            return;
        }

        if !self.rhunk_active {
            self.rhunk_active = true;
            self.rhunk_has_changes = false;
            self.rhunk_new_begin = lno_0 + 1;
            self.rhunk_old_begin =
                if self.pending_rm_count != 0 { self.pending_rm_pre_begin } else { cur_pre };
            self.rhunk_old_count = 0;
            self.rhunk_new_count = 0;
            self.rhunk.clear();
        }
        if self.pending_rm_count != 0 {
            let pending = std::mem::take(&mut self.pending_rm);
            self.rhunk.extend_from_slice(&pending);
            self.rhunk_old_count += self.pending_rm_count;
            self.rhunk_has_changes = true;
            self.pending_rm_count = 0;
        }
        self.rhunk.extend_from_slice(line);
        self.rhunk_new_count += 1;
        if line.first() == Some(&b'+') {
            self.rhunk_has_changes = true;
        } else {
            self.rhunk_old_count += 1;
        }
    }
}

impl gix::diff::blob::unified_diff::ConsumeHunk for RangeSink<'_> {
    type Out = Vec<u8>;

    fn consume_hunk(
        &mut self,
        header: gix::diff::blob::unified_diff::HunkHeader,
        lines: &[(gix::diff::blob::unified_diff::DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        use gix::diff::blob::unified_diff::DiffLineKind;

        // The heading, computed exactly as the unfiltered patch sink does: the
        // nearest qualifying line at or above the hunk's first pre-image line,
        // searched no further back than the previous hunk started.
        let s1 = header.before_hunk_start as i64 - 1;
        if let Some(f) = func_line(self.before, s1 - 1, self.func_prev) {
            self.func_text = f.to_vec();
        }
        self.func_prev = s1 - 1;
        self.func = self.func_text.clone();

        // `xdl_emit_hunk_hdr` hands a zero-count side its start minus one; no line of
        // that kind follows, so only the header arithmetic sees it.
        self.lno_pre = if header.before_hunk_len != 0 {
            header.before_hunk_start as i64
        } else {
            header.before_hunk_start as i64 - 1
        };
        self.lno_post = if header.after_hunk_len != 0 {
            header.after_hunk_start as i64
        } else {
            header.after_hunk_start as i64 - 1
        };

        let mut bi = header.before_hunk_start.saturating_sub(1) as usize;
        let mut ai = header.after_hunk_start.saturating_sub(1) as usize;
        let mut rec: Vec<u8> = Vec::new();
        for (kind, fallback) in lines {
            let (marker, content): (u8, &[u8]) = match kind {
                // `xdl_emit_diff()` emits every context record from `xe->xdf2`, the
                // *post-image* — all three context loops call
                // `xdl_emit_record(&xe->xdf2, s2, " ", ecb)`. It only matters when
                // the two sides hold different bytes for a record the comparison
                // called equal, which is exactly what `-w`/`-b`/
                // `--ignore-space-at-eol` do; reading it off `before` here made
                // `git log -L <range>:<file> -w` print the pre-image's indentation
                // for its context lines where stock prints the post-image's.
                DiffLineKind::Context => {
                    let c = self.after.get(ai).copied().unwrap_or(*fallback);
                    bi += 1;
                    ai += 1;
                    (b' ', c)
                }
                DiffLineKind::Remove => {
                    let c = self.before.get(bi).copied().unwrap_or(*fallback);
                    bi += 1;
                    (b'-', c)
                }
                DiffLineKind::Add => {
                    let c = self.after.get(ai).copied().unwrap_or(*fallback);
                    ai += 1;
                    (b'+', c)
                }
            };
            rec.clear();
            rec.push(marker);
            rec.extend_from_slice(content);
            // xdiff emits the missing terminator and the marker as their own records,
            // which is why the filter sees `\` as a separate line.
            let incomplete = content.last() != Some(&b'\n');
            if incomplete {
                rec.push(b'\n');
            }
            let rec_line = std::mem::take(&mut rec);
            self.line(&rec_line);
            rec = rec_line;
            if incomplete {
                self.line(b"\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(mut self) -> Vec<u8> {
        self.flush_rhunk();
        self.buf
    }
}
