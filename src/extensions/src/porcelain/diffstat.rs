//! `diff.c`'s diffstat renderer, in one place.
//!
//! Every `--stat` in the diff family — `diff`, `diff-files`, `diff-index`,
//! `diff-tree`, `log`, `show`, `format-patch`, `merge`, `request-pull`,
//! `bisect` — is the same C function, [`show_stats`] (`diff.c:2953`), fed a
//! `struct diffstat_file` list. It had been re-typed once per caller, and the
//! copies had drifted into three different answers for the name column alone
//! (bytes, Unicode scalars, and nothing at all for `--stat-count`). This module
//! is the single port; callers build [`StatFile`] rows and hand them over.
//!
//! `apply.c` has a `show_stats()` of its own with different arithmetic (a 50/70
//! column split and round-half-up scaling); that one is *not* this function and
//! stays in [`super::apply`].
//!
//! The name column is measured in **display columns** (`utf8_strwidth()`), which
//! is what the C does at `diff.c:2985` and `:3113`. Counting bytes puts the `|`
//! past the terminal edge for any non-ASCII name that `core.quotePath=false`
//! leaves un-escaped; counting Unicode scalars gets CJK wrong by one column per
//! glyph. Both were live before this module existed.

use crate::utf8::{utf8_strnwidth, utf8_width};

use super::diff_color::{self, DiffColors, DiffSlot};

/// One `struct diffstat_file`, reduced to what [`show_stats`] reads.
pub(crate) struct StatFile {
    /// `file->print_name`: the result of `fill_print_name()` — the C-quoted path
    /// (or `pprint_rename()`'s `pfx{a => b}sfx` form), plus the
    /// `--compact-summary` ` (<comment>)` annotation when one applies.
    ///
    /// Callers own the quoting because they own the name: only they know whether
    /// the pair is a rename and what its comment is.
    pub(crate) print_name: Vec<u8>,
    /// Added lines, or the new side's byte count when `binary`.
    pub(crate) added: u64,
    /// Deleted lines, or the old side's byte count when `binary`.
    pub(crate) deleted: u64,
    pub(crate) binary: bool,
    pub(crate) is_unmerged: bool,
}

impl StatFile {
    /// A plain text row.
    pub(crate) fn text(print_name: Vec<u8>, added: u64, deleted: u64) -> Self {
        StatFile { print_name, added, deleted, binary: false, is_unmerged: false }
    }
}

/// The `--stat` geometry, in git's own sentinel encoding.
///
/// `width` is `options->stat_width`: `-1` == "the terminal width"
/// ([`crate::pager::term_columns`]), `0` == "the 80-column default", positive ==
/// that width.
/// `name_width`/`graph_width` are `options->stat_{name,graph}_width`: `-1` ==
/// "unset, take the `diff.stat*Width` config", `0` == uncapped. `count` is
/// `options->stat_count`: `0` == every file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct StatWidths {
    pub(crate) width: i64,
    pub(crate) name_width: i64,
    pub(crate) graph_width: i64,
    pub(crate) count: i64,
}

impl Default for StatWidths {
    fn default() -> Self {
        StatWidths { width: -1, name_width: -1, graph_width: -1, count: 0 }
    }
}

impl StatWidths {
    /// What `repo_diff_setup()` alone leaves behind: an all-zero geometry.
    ///
    /// `init_diffstat_widths()` — the call that turns the three widths into the
    /// `-1` "ask the terminal / ask the config" sentinels — is made by exactly
    /// three builtins: `builtin/diff.c:510`, `builtin/log.c:209`
    /// (`log`/`show`/`whatchanged`) and `builtin/merge.c:515`. Every other verb
    /// that renders a diffstat — `diff-tree`, `diff-index`, `diff-files`,
    /// `diff-pairs`, and `bisect`'s `diff-tree` emulation — reaches
    /// `show_stats()` with these zeros, so it renders at a flat 80 columns and
    /// never reads `$COLUMNS`, `diff.statNameWidth` or `diff.statGraphWidth`.
    pub(crate) fn plumbing() -> Self {
        StatWidths { width: 0, name_width: 0, graph_width: 0, count: 0 }
    }
}

/// Parse `--stat=<width>[,<name-width>[,<count>]]` (`diff_opt_stat()`): each
/// present, numeric field overwrites the corresponding slot; an empty or
/// non-numeric field is left unchanged, which is byte-equivalent to git's
/// `strtoul` (empty == `0` == unset).
pub(crate) fn parse_stat_geometry(sw: &mut StatWidths, spec: &str) {
    let mut it = spec.split(',');
    if let Some(w) = it.next() {
        if let Ok(v) = w.trim().parse::<i64>() {
            sw.width = v;
        }
    }
    if let Some(n) = it.next() {
        if let Ok(v) = n.trim().parse::<i64>() {
            sw.name_width = v;
        }
    }
    if let Some(c) = it.next() {
        if let Ok(v) = c.trim().parse::<i64>() {
            sw.count = v;
        }
    }
}

/// `decimal_width()` (`diff.c`): the number of digits `n` prints as.
pub(crate) fn decimal_width(mut n: u64) -> i64 {
    let mut w = 1i64;
    while n >= 10 {
        n /= 10;
        w += 1;
    }
    w
}

/// `scale_linear()` (`diff.c:2839`).
///
/// > make sure that at least one '-' or '+' is printed if there is any change to
/// > this path. The easiest way is to scale linearly as if the allotted width is
/// > one column shorter than it is, and then add 1 to the result.
///
/// The C divides by `max_change` without guarding it, because its only caller
/// is behind `if (graph_width <= max_change)` and `graph_width` is never
/// negative — so a zero `max_change` cannot reach the division. Rust would
/// panic rather than trap, so the guard is spelled out; it is unreachable for
/// the same reason it is in C.
pub(crate) fn scale_linear(it: i64, width: i64, max_change: i64) -> i64 {
    if it == 0 || max_change == 0 {
        return 0;
    }
    1 + (it * (width - 1) / max_change)
}

/// `utf8_strwidth()` (`utf8.c:236`): `utf8_strnwidth(s, strlen(s), 0)` — display
/// columns, ANSI sequences *not* skipped, invalid UTF-8 answering the byte
/// length.
pub(crate) fn strwidth(s: &[u8]) -> i64 {
    i64::from(utf8_strnwidth(s, false))
}

/// `utf8_ish_width()` (`diff.c:2934`): `utf8_width()` made safe for a loop that
/// subtracts per-character widths — invalid UTF-8 advances one byte and answers
/// 1, and a control character answers 0 rather than -1.
fn utf8_ish_width(s: &[u8], pos: &mut usize) -> i64 {
    let old = *pos;
    match utf8_width(s, pos) {
        None => {
            *pos = old + 1;
            1
        }
        Some(w) => i64::from(w.max(0)),
    }
}

/// `print_stat_summary_inserts_deletes()` (`diff.c:2880`): the trailing
/// ` N files changed, A insertions(+), D deletions(-)` line, which is also the
/// whole of `--shortstat`.
pub(crate) fn print_stat_summary(out: &mut Vec<u8>, files: u64, insertions: u64, deletions: u64) {
    if files == 0 {
        out.extend_from_slice(b" 0 files changed\n");
        return;
    }
    out.extend_from_slice(
        format!(" {files} file{} changed", if files == 1 { "" } else { "s" }).as_bytes(),
    );
    // "For binary diff, the caller may want to print "x files changed" with
    // insertions == 0 && deletions == 0."
    if insertions != 0 || deletions == 0 {
        out.extend_from_slice(
            format!(", {insertions} insertion{}(+)", if insertions == 1 { "" } else { "s" })
                .as_bytes(),
        );
    }
    if deletions != 0 || insertions == 0 {
        out.extend_from_slice(
            format!(", {deletions} deletion{}(-)", if deletions == 1 { "" } else { "s" }).as_bytes(),
        );
    }
    out.push(b'\n');
}

/// The resolved column geometry, split out so it can be asserted directly.
struct Geometry {
    /// How many rows get a line — git's `count` after the scan loop clamps it.
    count: usize,
    name_width: i64,
    number_width: i64,
    graph_width: i64,
    max_change: i64,
}

/// The first half of `show_stats()`: scan the shown rows, then divide the
/// available width between the name column and the graph.
fn geometry(files: &[StatFile], sw: &StatWidths) -> Geometry {
    // `count = options->stat_count ? options->stat_count : data->nr`.
    let count: i64 = if sw.count != 0 { sw.count } else { files.len() as i64 };

    let mut max_change: i64 = 0;
    let mut max_len: i64 = 0;
    let mut bin_width: i64 = 0;
    let mut number_width: i64 = 0;
    let mut i: i64 = 0;
    while i < count && i < files.len() as i64 {
        let f = &files[i as usize];
        i += 1;
        // git's `!file->is_interesting && change == 0` skip (which bumps `count`
        // to make room for one more) cannot fire for any caller here: every row
        // these callers build comes from a pair with a real status, which is
        // exactly what sets `is_interesting`.
        max_len = max_len.max(strwidth(&f.print_name));
        if f.is_unmerged {
            bin_width = bin_width.max(8); // "Unmerged" is 8 characters.
            continue;
        }
        if f.binary {
            // "Bin XXX -> YYY bytes"
            let w = 14 + decimal_width(f.added) + decimal_width(f.deleted);
            bin_width = bin_width.max(w);
            // "Display change counts aligned with "Bin"".
            number_width = 3;
            continue;
        }
        max_change = max_change.max((f.added + f.deleted) as i64);
    }
    // `count = i`: where we can stop scanning in data->files[].
    let count = i.max(0) as usize;

    // `if (options->stat_width == -1) width = term_columns() - <line prefix>;
    //  else width = options->stat_width ? options->stat_width : 80;`
    //
    // The `line_prefix` subtraction is `--graph`/`--line-prefix` territory; no
    // caller here renders a stat behind one, so it is the empty string and the
    // subtraction is zero.
    let mut width: i64 = if sw.width == -1 {
        crate::pager::term_columns()
    } else if sw.width != 0 {
        sw.width
    } else {
        80
    };
    number_width = number_width.max(decimal_width(max_change.max(0) as u64));

    // `-1` here is "no `--stat-name-width`/`--stat-graph-width` and no
    // `diff.stat*Width` config", which is the same as "uncapped" == 0.
    let stat_name_width = if sw.name_width == -1 { 0 } else { sw.name_width };
    let stat_graph_width = if sw.graph_width == -1 { 0 } else { sw.graph_width };

    // "Guarantee 3/8*16 == 6 for the graph part and 5/8*16 == 10 for the
    // filename part".
    if width < 16 + 6 + number_width {
        width = 16 + 6 + number_width;
    }

    // "strlen("Bin XXX -> YYY bytes") == bin_width, and the part starting from
    // "XXX" should fit in graph_width."
    let mut graph_width = if max_change + 4 > bin_width { max_change } else { bin_width - 4 };
    if stat_graph_width > 0 && stat_graph_width < graph_width {
        graph_width = stat_graph_width;
    }
    let mut name_width =
        if stat_name_width > 0 && stat_name_width < max_len { stat_name_width } else { max_len };

    // "Adjust adjustable widths not to exceed maximum width".
    if name_width + number_width + 6 + graph_width > width {
        if graph_width > width * 3 / 8 - number_width - 6 {
            graph_width = width * 3 / 8 - number_width - 6;
            if graph_width < 6 {
                graph_width = 6;
            }
        }
        if stat_graph_width > 0 && graph_width > stat_graph_width {
            graph_width = stat_graph_width;
        }
        if name_width > width - number_width - 6 - graph_width {
            name_width = width - number_width - 6 - graph_width;
        } else {
            graph_width = width - number_width - 6 - name_width;
        }
    }

    Geometry { count, name_width, number_width, graph_width, max_change }
}

/// `"scale" the filename`: a name wider than the column keeps its tail behind a
/// `...` prefix, cut back to a `/` boundary when one falls inside the tail.
///
/// The C walks glyphs off the *front* with `utf8_ish_width()` until the
/// remaining display width fits, so the cut lands on a character boundary and a
/// wide glyph costs two columns. Slicing `name_width - 3` bytes off the end — as
/// the byte-counting copies did — cuts mid-sequence for any multibyte name.
fn scale_name(full: &[u8], name_width: i64) -> (&'static str, &[u8]) {
    let name_len = strwidth(full);
    if name_width >= name_len {
        return ("", full);
    }
    let len = (name_width - 3).max(0);
    let mut name_len = name_len;
    let mut at = 0usize;
    while name_len > len && at < full.len() {
        name_len -= utf8_ish_width(full, &mut at);
    }
    let tail = &full[at..];
    // `slash = strchr(name, '/'); if (slash) name = slash;`
    match tail.iter().position(|&b| b == b'/') {
        Some(p) => ("...", &tail[p..]),
        None => ("...", tail),
    }
}

/// `show_graph()` (`diff.c:2852`): a run of `cnt` copies of `ch`, wrapped in its
/// own color pair, and nothing at all when the run is empty.
fn show_graph(out: &mut Vec<u8>, ch: u8, cnt: i64, colors: &DiffColors, slot: DiffSlot) {
    if cnt <= 0 {
        return;
    }
    diff_color::paint(out, colors, slot, &vec![ch; cnt as usize]);
}

/// `show_stats()` (`diff.c:2953`) followed by
/// `print_stat_summary_inserts_deletes()`.
///
/// `files` is the whole diffstat: the summary line tallies all of it even when
/// `--stat-count` cuts the listing short.
pub(crate) fn show_stats(out: &mut Vec<u8>, files: &[StatFile], sw: &StatWidths, colors: &DiffColors) {
    if files.is_empty() {
        return;
    }
    let g = geometry(files, sw);

    for f in files.iter().take(g.count) {
        let (prefix, name) = scale_name(&f.print_name, g.name_width);
        let padding = (g.name_width - prefix.len() as i64 - strwidth(name)).max(0) as usize;
        let nw = g.number_width.max(0) as usize;

        out.push(b' ');
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&b" ".repeat(padding));
        out.extend_from_slice(b" | ");

        if f.binary {
            out.extend_from_slice(format!("{:>nw$}", "Bin").as_bytes());
            if f.added == 0 && f.deleted == 0 {
                out.push(b'\n');
                continue;
            }
            // The two byte counts are painted with the old/new colors.
            out.push(b' ');
            diff_color::paint(out, colors, DiffSlot::Old, f.deleted.to_string().as_bytes());
            out.extend_from_slice(b" -> ");
            diff_color::paint(out, colors, DiffSlot::New, f.added.to_string().as_bytes());
            out.extend_from_slice(b" bytes\n");
            continue;
        }
        if f.is_unmerged {
            // The C's format string is `%*s` over the literal `"Unmerged\n"`, so
            // the newline is inside the padded field and a `number_width` above
            // 9 pads to the left of the word.
            out.extend_from_slice(format!("{:>nw$}", "Unmerged\n").as_bytes());
            continue;
        }

        let (mut add, mut del) = (f.added as i64, f.deleted as i64);
        if g.graph_width <= g.max_change {
            let mut total = scale_linear(add + del, g.graph_width, g.max_change);
            if total < 2 && add > 0 && del > 0 {
                // "width >= 2 due to the sanity check".
                total = 2;
            }
            if add < del {
                add = scale_linear(add, g.graph_width, g.max_change);
                del = total - add;
            } else {
                del = scale_linear(del, g.graph_width, g.max_change);
                add = total - del;
            }
        }
        let change = f.added + f.deleted;
        out.extend_from_slice(format!("{change:>nw$}").as_bytes());
        if change != 0 {
            out.push(b' ');
        }
        show_graph(out, b'+', add, colors, DiffSlot::New);
        show_graph(out, b'-', del, colors, DiffSlot::Old);
        out.push(b'\n');
    }

    // The tally walks every file, not just the listed ones, and emits
    // `DIFF_SYMBOL_STATS_SUMMARY_ABBREV` once if any were hidden.
    let mut total_files = files.len() as i64;
    let (mut adds, mut dels) = (0u64, 0u64);
    let mut extra_shown = false;
    for (i, f) in files.iter().enumerate() {
        if f.is_unmerged {
            total_files -= 1;
            continue;
        }
        if !f.binary {
            adds += f.added;
            dels += f.deleted;
        }
        if i < g.count {
            continue;
        }
        if !extra_shown {
            out.extend_from_slice(b" ...\n");
        }
        extra_shown = true;
    }

    print_stat_summary(out, total_files.max(0) as u64, adds, dels);
}

/// `show_shortstats()` (`diff.c:3221`): the summary line alone, over every file.
/// An unmerged entry is not counted, and a binary one contributes no lines.
pub(crate) fn show_shortstats(out: &mut Vec<u8>, files: &[StatFile]) {
    if files.is_empty() {
        return;
    }
    let mut total_files = files.len() as i64;
    let (mut adds, mut dels) = (0u64, 0u64);
    for f in files {
        if f.is_unmerged {
            total_files -= 1;
            continue;
        }
        if !f.binary {
            adds += f.added;
            dels += f.deleted;
        }
    }
    print_stat_summary(out, total_files.max(0) as u64, adds, dels);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(files: &[StatFile], sw: &StatWidths) -> String {
        let mut out = Vec::new();
        show_stats(&mut out, files, sw, &DiffColors::disabled());
        String::from_utf8(out).expect("ascii/utf-8 fixture")
    }

    /// Every expectation below was read off stock git 2.55.0 first; see the
    /// module header for why the name column is display columns.
    #[test]
    fn name_column_is_display_columns_not_bytes() {
        // "dir/café.txt" is 12 columns and 13 bytes. Measured in bytes the `|`
        // lands one column late.
        let files = vec![
            StatFile::text("dir/café.txt".as_bytes().to_vec(), 1, 0),
            StatFile::text(b"dir/plain.txt".to_vec(), 1, 0),
        ];
        let got = render(&files, &StatWidths::plumbing());
        assert_eq!(
            got,
            " dir/café.txt  | 1 +\n dir/plain.txt | 1 +\n 2 files changed, 2 insertions(+)\n"
        );
    }

    /// A CJK glyph is two columns wide, which neither a byte count (3) nor a
    /// scalar count (1) gets right.
    #[test]
    fn wide_glyphs_cost_two_columns() {
        let files = vec![
            StatFile::text("日本.txt".as_bytes().to_vec(), 1, 0),
            StatFile::text(b"12345678".to_vec(), 1, 0),
        ];
        // "日本.txt" is 4 + 4 == 8 columns, the same as "12345678".
        let got = render(&files, &StatWidths::plumbing());
        assert_eq!(
            got,
            " 日本.txt | 1 +\n 12345678 | 1 +\n 2 files changed, 2 insertions(+)\n"
        );
    }

    /// `--stat-count` cuts the listing and adds ` ...`, and it narrows the
    /// columns too because the geometry scan stops at the same place.
    #[test]
    fn stat_count_truncates_and_marks() {
        let files = vec![
            StatFile::text(b"a".to_vec(), 1, 0),
            StatFile::text(b"bbbbbbbbbbbbbbbbbbbb".to_vec(), 2, 0),
        ];
        let sw = StatWidths { count: 1, ..StatWidths::plumbing() };
        assert_eq!(render(&files, &sw), " a | 1 +\n ...\n 2 files changed, 3 insertions(+)\n");
    }

    /// An unmerged row prints "Unmerged" and is left out of the file tally.
    #[test]
    fn unmerged_rows_are_named_and_uncounted() {
        let files = vec![
            StatFile { print_name: b"conflict".to_vec(), added: 0, deleted: 0, binary: false, is_unmerged: true },
            StatFile::text(b"other".to_vec(), 1, 0),
        ];
        let got = render(&files, &StatWidths::plumbing());
        assert_eq!(
            got,
            " conflict | Unmerged\n other    | 1 +\n 1 file changed, 1 insertion(+)\n"
        );
    }

    /// The `...` elision walks whole glyphs off the front until the remaining
    /// display width fits `name_width - 3`, then resumes at the first `/` inside
    /// what is left. Slicing bytes instead cuts a multibyte sequence in half.
    ///
    /// Read off stock git 2.55.0 as
    /// `git -c core.quotePath=false diff --stat-name-width=20 --stat` over
    /// `aaa/日本語のディレクトリ/file.txt`:
    /// `' .../file.txt         | 1 +\n'`.
    #[test]
    fn elision_cuts_on_glyph_boundaries() {
        let name = "aaa/日本語のディレクトリ/file.txt".as_bytes().to_vec();
        let sw = StatWidths { name_width: 20, ..StatWidths::plumbing() };
        let got = render(&[StatFile::text(name, 1, 0)], &sw);
        assert_eq!(got, " .../file.txt         | 1 +\n 1 file changed, 1 insertion(+)\n");
    }

    /// `scale_linear` never divides by zero, and answers at least one column for
    /// any non-zero change.
    #[test]
    fn scale_linear_matches_the_c() {
        assert_eq!(scale_linear(0, 10, 100), 0);
        assert_eq!(scale_linear(5, 10, 0), 0);
        assert_eq!(scale_linear(1, 10, 100), 1);
        assert_eq!(scale_linear(100, 10, 100), 10);
        // Identity when the graph is exactly as wide as the largest change.
        for it in 0..=20 {
            assert_eq!(scale_linear(it, 20, 20), it);
        }
    }

    #[test]
    fn decimal_width_counts_digits() {
        assert_eq!(decimal_width(0), 1);
        assert_eq!(decimal_width(9), 1);
        assert_eq!(decimal_width(10), 2);
        assert_eq!(decimal_width(999), 3);
        assert_eq!(decimal_width(1000), 4);
    }
}
