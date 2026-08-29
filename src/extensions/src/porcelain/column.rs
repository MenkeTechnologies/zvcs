//! `git column` — format stdin lines into a table with multiple columns.
//!
//! A direct port of upstream `builtin/column.c` plus the layout engine in
//! `column.c`: `parse_option`/`parse_config` (the `column.ui` token language),
//! `finalize_colopts`, `layout`, `compute_column_width`, `shrink_columns`,
//! `display_plain` and `display_cell`. The cell-placement arithmetic
//! (`XY2LINEAR`, `DIV_ROUND_UP`, the trailing-space suppression on the last
//! cell of a line) is reproduced instruction-for-instruction, so stdout is
//! byte-identical for the covered inputs.
//!
//! Covered: `--command=<name>`, `--mode[=<style>]` / `--no-mode`, `--raw-mode=<n>`,
//! `--width=<n>` / `--no-width`, `--indent=<string>`, `--nl=<string>`,
//! `--padding=<n>`, `-h`; the `always`/`never`/`auto`/`plain`/`column`/`row`/
//! `dense`/`nodense` mode tokens and the "layout implies always" rule; the
//! `column.ui` and `column.<command>` config variables; unambiguous long-option
//! abbreviation; `k`/`m`/`g` suffixes on numeric values; CRLF-tolerant stdin
//! splitting; and the exit codes 129 (usage/parse error) and 128 (`--padding`
//! negative, `--command` misplaced, bad config value).
//!
//! Two deliberate, documented divergences from stock git:
//!
//!   * Cell width counts Unicode scalar values, after stripping SGR escape
//!     sequences exactly as git's `display_mode_esc_sequence_len` does, and
//!     after dropping the glyphs `git_wcwidth()` scores at or below zero by its
//!     own first test — NUL and the control ranges. git additionally applies the
//!     `unicode-width.h` tables, so lines containing combining marks or wide
//!     (CJK) characters lay out differently. This matches the convention already
//!     used by `shortlog` and `request-pull` in this crate.
//!   * [`crate::pager::term_columns`] (used only when `--width` is absent or
//!     zero) reads `COLUMNS` with C's `atoi` and otherwise falls back to 80; the
//!     `TIOCGWINSZ` probe is not performed, so an unset `COLUMNS` on a terminal
//!     of a different size lays out at 80 columns rather than the real width.
//!
//! Not covered: nothing else — every remaining flag git accepts is implemented,
//! and unknown flags produce git's own `unknown option` usage error rather than
//! being ignored.

use anyhow::{bail, Result};

// git's `term_columns()` (`pager.c:203`), shared with every other consumer — see
// `crate::pager::term_columns` for the `atoi` grammar and for why the
// `TIOCGWINSZ` probe is absent.
use crate::pager::term_columns;
use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;

// Bit layout of `colopts`, verbatim from upstream `column.h`.
const COL_LAYOUT_MASK: u32 = 0x000F;
const COL_ENABLE_MASK: u32 = 0x0030;
const COL_PARSEOPT: u32 = 0x0040;
const COL_DENSE: u32 = 0x0080;
const COL_DISABLED: u32 = 0x0000;
const COL_ENABLED: u32 = 0x0010;
const COL_AUTO: u32 = 0x0020;
const COL_COLUMN: u32 = 0;
const COL_ROW: u32 = 1;
const COL_PLAIN: u32 = 15;

// `parse_option`'s "which group did the user touch" bookkeeping.
const LAYOUT_SET: u32 = 1;
const ENABLE_SET: u32 = 2;

/// Byte-exact reproduction of `builtin_column_usage` as rendered by
/// `parse-options`, including the blank line that closes the option block.
const USAGE: &str = "\
usage: git column [<options>]

    --[no-]command <name> lookup config vars
    --[no-]mode[=<style>] layout to use
    --raw-mode <n>        layout to use
    --[no-]width <n>      maximum width
    --[no-]indent <string>
                          padding space on left border
    --[no-]nl <string>    padding space on right border
    --[no-]padding <n>    padding space between columns

";

/// The resolved `struct column_options`. `width == 0` means "ask the terminal";
/// `indent`/`nl` being `None` is git's NULL, which `print_columns` defaults.
struct Options {
    width: i64,
    padding: i64,
    indent: Option<String>,
    nl: Option<String>,
}

/// How many arguments a long option consumes, mirroring the `parse-options`
/// flags on each entry of `builtin/column.c`'s option table.
#[derive(Clone, Copy, PartialEq)]
enum ArgKind {
    /// `--opt=<v>` only; bare `--opt` is valid and passes no value.
    Optional,
    /// `--opt=<v>` or `--opt <v>`.
    Required,
}

/// One entry of the option table: name, argument shape, and whether git accepts
/// the `--no-` form (`--raw-mode` is `OPT_UNSIGNED`, which forbids negation).
struct Spec {
    name: &'static str,
    arg: ArgKind,
    negatable: bool,
}

const SPECS: [Spec; 7] = [
    Spec { name: "command",  arg: ArgKind::Required, negatable: true },
    Spec { name: "mode",     arg: ArgKind::Optional, negatable: true },
    Spec { name: "raw-mode", arg: ArgKind::Required, negatable: false },
    Spec { name: "width",    arg: ArgKind::Required, negatable: true },
    Spec { name: "indent",   arg: ArgKind::Required, negatable: true },
    Spec { name: "nl",       arg: ArgKind::Required, negatable: true },
    Spec { name: "padding",  arg: ArgKind::Required, negatable: true },
];

/// `git column` — display data in columns.
///
/// Reads every line of stdin as one table cell, then renders per the layout
/// selected by config (`column.ui`, `column.<command>`) and the command line.
/// A run with no input lines prints nothing and exits 0, as stock git does.
pub fn column(args: &[String]) -> Result<ExitCode> {
    // The dispatcher passes the argument tail; tolerate the subcommand being
    // present at index 0 so both calling conventions behave identically.
    let args: &[String] = match args.first() {
        Some(a) if a == "column" => &args[1..],
        _ => args,
    };

    // `--command=<name>` is special: `cmd_column` inspects `argv[1]` *before*
    // reading config, so only the very first argument selects a config section.
    let command: Option<String> = args
        .first()
        .and_then(|a| a.strip_prefix("--command="))
        .map(str::to_string);

    let mut colopts: u32 = COL_DISABLED;
    if let Err(err) = apply_config(&mut colopts, command.as_deref()) {
        eprint!("{err}");
        return Ok(ExitCode::from(128));
    }

    let mut opts = Options { width: 0, padding: 1, indent: None, nl: None };
    let mut real_command: Option<String> = None;

    match parse_args(args, &mut colopts, &mut opts, &mut real_command) {
        Ok(Outcome::Parsed) => {}
        Ok(Outcome::Help) => {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        Err(ParseError::Usage(msg)) => {
            eprint!("{msg}{USAGE}");
            return Ok(ExitCode::from(129));
        }
        Err(ParseError::Bare(msg)) => {
            eprint!("{msg}");
            return Ok(ExitCode::from(129));
        }
    }

    if opts.padding < 0 {
        eprintln!("fatal: --padding must be non-negative");
        return Ok(ExitCode::from(128));
    }
    // git compares the pointer-derived `command` against the parsed value: they
    // agree only when `--command=<name>` was the first argument and was not
    // later overridden or negated.
    if (real_command.is_some() || command.is_some()) && real_command != command {
        eprintln!("fatal: --command must be the first argument");
        return Ok(ExitCode::from(128));
    }

    finalize_colopts(&mut colopts);

    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let list = split_lines(&input);

    let mut out: Vec<u8> = Vec::new();
    print_columns(&list, colopts, &opts, &mut out)?;
    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// What a successful argument scan produced.
enum Outcome {
    Parsed,
    /// `-h`: usage goes to stdout and the exit code is still 129.
    Help,
}

/// A parse failure. `Usage` appends the option block (git's
/// `usage_with_options`); `Bare` does not (git's `PARSE_OPT_ERROR`).
enum ParseError {
    Usage(String),
    Bare(String),
}

/// Port of the `parse_options` pass over `builtin/column.c`'s option table.
fn parse_args(
    args: &[String],
    colopts: &mut u32,
    opts: &mut Options,
    real_command: &mut Option<String>,
) -> Result<Outcome, ParseError> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();

        // `--help-all` is a `strcmp()` of its own inside `parse_options_step()`,
        // rendering `USAGE_FULL` — the same block `-h` prints, this table having
        // no `PARSE_OPT_HIDDEN` entry. It never abbreviates and never takes an
        // `=<value>`, so it is matched whole rather than resolved below.
        if a == "-h" || a == "--help" || a == "--help-all" {
            return Ok(Outcome::Help);
        }
        if a == "--" {
            // Everything after `--` is positional, and `git column` takes none.
            if i + 1 < args.len() {
                return Err(ParseError::Usage(String::new()));
            }
            break;
        }
        if !a.starts_with("--") {
            // A non-option argument, or an unknown short option: both end in
            // `usage_with_options`, the latter after an error line.
            if a.starts_with('-') && a.len() > 1 {
                return Err(ParseError::Usage(format!(
                    "error: unknown switch `{}'\n",
                    &a[1..2]
                )));
            }
            return Err(ParseError::Usage(String::new()));
        }

        let body = &a[2..];
        let (token, inline) = match body.split_once('=') {
            Some((t, v)) => (t, Some(v)),
            None => (body, None),
        };

        let (spec, negated) = resolve(token, a)?;
        if negated {
            // `--no-<opt>` never takes a value.
            match spec.name {
                "command" => *real_command = None,
                "mode" => {
                    *colopts |= COL_PARSEOPT;
                    *colopts &= !COL_ENABLE_MASK;
                }
                "width" => opts.width = 0,
                "indent" => opts.indent = None,
                "nl" => opts.nl = None,
                "padding" => opts.padding = 0,
                _ => unreachable!("resolve() only returns negatable specs here"),
            }
            i += 1;
            continue;
        }

        // Pull the value: inline after `=`, else the next argv entry for
        // required-argument options.
        let value: Option<String> = match (spec.arg, inline) {
            (_, Some(v)) => Some(v.to_string()),
            (ArgKind::Optional, None) => None,
            (ArgKind::Required, None) => {
                i += 1;
                match args.get(i) {
                    Some(v) => Some(v.clone()),
                    None => {
                        return Err(ParseError::Usage(format!(
                            "error: option `{}' requires a value\n",
                            spec.name
                        )))
                    }
                }
            }
        };

        match spec.name {
            "command" => *real_command = value,
            "mode" => {
                // `parseopt_column_callback`: `--mode` alone means "always"
                // with the layout left at its default.
                *colopts |= COL_PARSEOPT;
                *colopts &= !COL_ENABLE_MASK;
                *colopts |= COL_ENABLED;
                if let Some(v) = value {
                    parse_config(colopts, &v)
                        .map_err(|e| ParseError::Bare(format!("error: {e}\n")))?;
                }
            }
            "raw-mode" => {
                let v = value.expect("required argument");
                *colopts = parse_unsigned(&v, "raw-mode")? as u32;
            }
            "width" => opts.width = parse_integer(&value.expect("required argument"), "width")?,
            "padding" => {
                opts.padding = parse_integer(&value.expect("required argument"), "padding")?
            }
            "indent" => opts.indent = value,
            "nl" => opts.nl = value,
            _ => unreachable!("all specs handled"),
        }
        i += 1;
    }
    Ok(Outcome::Parsed)
}

/// Match `token` against the option table: exact name first, then unambiguous
/// abbreviation, in both the plain and `--no-` forms — git's `parse_long_opt`.
fn resolve(token: &str, whole: &str) -> Result<(&'static Spec, bool), ParseError> {
    let unknown = || ParseError::Usage(format!("error: unknown option `{}'\n", &whole[2..]));

    for negated in [false, true] {
        let stem = if negated {
            match token.strip_prefix("no-") {
                Some(s) => s,
                None => continue,
            }
        } else {
            token
        };
        let usable = |s: &Spec| !negated || s.negatable;

        if let Some(spec) = SPECS.iter().find(|s| s.name == stem && usable(s)) {
            return Ok((spec, negated));
        }
        let candidates: Vec<&Spec> = SPECS
            .iter()
            .filter(|s| s.name.starts_with(stem) && usable(s))
            .collect();
        match candidates.len() {
            0 => continue,
            1 => return Ok((candidates[0], negated)),
            _ => {
                let dash = if negated { "--no-" } else { "--" };
                let list: Vec<String> =
                    candidates.iter().map(|s| format!("{dash}{}", s.name)).collect();
                return Err(ParseError::Bare(format!(
                    "error: ambiguous option: {stem} (could be {})\n",
                    list.join(" or ")
                )));
            }
        }
    }
    Err(unknown())
}

/// `OPT_INTEGER`: a decimal value with an optional `k`/`m`/`g` binary suffix.
fn parse_integer(value: &str, name: &str) -> Result<i64, ParseError> {
    if value.is_empty() {
        return Err(ParseError::Bare(format!(
            "error: option `{name}' expects a numerical value\n"
        )));
    }
    parse_with_unit(value).ok_or_else(|| {
        ParseError::Bare(format!(
            "error: option `{name}' expects an integer value with an optional k/m/g suffix\n"
        ))
    })
}

/// `OPT_UNSIGNED`: as [`parse_integer`], but negative values are rejected too.
fn parse_unsigned(value: &str, name: &str) -> Result<i64, ParseError> {
    if value.is_empty() {
        return Err(ParseError::Bare(format!(
            "error: option `{name}' expects a numerical value\n"
        )));
    }
    parse_with_unit(value).filter(|n| *n >= 0).ok_or_else(|| {
        ParseError::Bare(format!(
            "error: option `{name}' expects a non-negative integer value with an optional \
             k/m/g suffix\n"
        ))
    })
}

/// git's `git_parse_signed`, through the shared [`crate::optint`] grammar:
/// `strtoimax` with base 0 followed by a binary unit suffix.
fn parse_with_unit(value: &str) -> Option<i64> {
    crate::optint::integer(&crate::optint::long_opt(""), value).ok()
}

/// Apply `column.ui` and, when `--command=<name>` led the command line,
/// `column.<name>` — git's `git_column_config` driven by the config reader.
///
/// The error string is git's own three-line report, minus the config file and
/// line number, which gitoxide's value lookup does not surface here.
fn apply_config(colopts: &mut u32, command: Option<&str>) -> Result<(), String> {
    let mut keys: Vec<String> = vec![COLUMN_UI.to_string()];
    if let Some(c) = command {
        // `column.ui` is matched first and returns early, so a `--command=ui`
        // never reaches the second branch.
        if c != "ui" {
            keys.push(format!("column.{c}"));
        }
    }
    apply_config_keys(colopts, &keys)
}

/// `column.ui`, always consulted first (`git_column_config`'s first branch).
const COLUMN_UI: &str = "column.ui";

/// The shared tail of [`apply_config`]: read each full `column.<name>` variable
/// in `keys` and fold its token list into `colopts`.
fn apply_config_keys(colopts: &mut u32, keys: &[String]) -> Result<(), String> {
    // git reads config whether or not there is a repository; fall back to the
    // global/system files when discovery fails.
    let values = match gix::discover(".") {
        Ok(repo) => read_values(repo.config_snapshot().plumbing(), keys),
        Err(_) => match gix::config::File::from_globals() {
            Ok(file) => read_values(&file, keys),
            Err(_) => Vec::new(),
        },
    };

    for (key, value) in values {
        if let Err(msg) = parse_config(colopts, &value) {
            return Err(format!(
                "error: {msg}\nerror: invalid {key} mode {value}\n\
                 fatal: bad config variable '{key}'\n"
            ));
        }
    }
    Ok(())
}

/// Collect every value of each full `column.<name>` key, in config order.
fn read_values(file: &gix::config::File, keys: &[String]) -> Vec<(String, String)> {
    let mut values = Vec::new();
    for key in keys {
        if let Some(found) = file.strings(key.as_str()) {
            for v in found {
                values.push((key.clone(), v.to_string()));
            }
        }
    }
    values
}

/// Port of `parse_config`: a space/comma separated token list, with the rule
/// that naming a layout but no enable state implies `always`.
fn parse_config(colopts: &mut u32, value: &str) -> Result<(), String> {
    let mut group_set = 0u32;
    for token in value.split([' ', ',']) {
        if token.is_empty() {
            continue;
        }
        parse_option(token, colopts, &mut group_set)?;
    }
    if group_set & LAYOUT_SET != 0 && group_set & ENABLE_SET == 0 {
        *colopts = (*colopts & !COL_ENABLE_MASK) | COL_ENABLED;
    }
    Ok(())
}

/// Port of `parse_option`: resolve one mode token against the `colopt` table.
fn parse_option(arg: &str, colopts: &mut u32, group_set: &mut u32) -> Result<(), String> {
    const TABLE: [(&str, u32, u32); 7] = [
        ("always", COL_ENABLED, COL_ENABLE_MASK),
        ("never", COL_DISABLED, COL_ENABLE_MASK),
        ("auto", COL_AUTO, COL_ENABLE_MASK),
        ("plain", COL_PLAIN, COL_LAYOUT_MASK),
        ("column", COL_COLUMN, COL_LAYOUT_MASK),
        ("row", COL_ROW, COL_LAYOUT_MASK),
        ("dense", COL_DENSE, 0),
    ];

    for (name, value, mask) in TABLE {
        // Only the maskless (boolean) entries accept a `no` prefix.
        let (candidate, set) = if mask == 0 && arg.len() > 2 && arg.starts_with("no") {
            (&arg[2..], false)
        } else {
            (arg, true)
        };
        if candidate != name {
            continue;
        }
        if mask == COL_ENABLE_MASK {
            *group_set |= ENABLE_SET;
        } else if mask == COL_LAYOUT_MASK {
            *group_set |= LAYOUT_SET;
        }
        if mask != 0 {
            *colopts = (*colopts & !mask) | value;
        } else if set {
            *colopts |= value;
        } else {
            *colopts &= !value;
        }
        return Ok(());
    }
    Err(format!("unsupported option '{arg}'"))
}

/// Port of `finalize_colopts` with `stdout_is_tty == -1`: resolve `auto`
/// against the real terminal state (and `GIT_PAGER_IN_USE`).
fn finalize_colopts(colopts: &mut u32) {
    if *colopts & COL_ENABLE_MASK == COL_AUTO {
        *colopts &= !COL_ENABLE_MASK;
        if std::io::stdout().is_terminal() || pager_in_use() {
            *colopts |= COL_ENABLED;
        }
    }
}

/// git's `pager_in_use()`: the `GIT_PAGER_IN_USE` environment flag.
fn pager_in_use() -> bool {
    match std::env::var("GIT_PAGER_IN_USE") {
        Ok(v) => !matches!(v.as_str(), "" | "0" | "false" | "no" | "off"),
        Err(_) => false,
    }
}

/// Split stdin the way a `strbuf_getline` loop does: on `\n`, dropping a `\r`
/// that immediately precedes it, and keeping a final unterminated line.
fn split_lines(input: &[u8]) -> Vec<Vec<u8>> {
    let mut list = Vec::new();
    let mut start = 0;
    while start < input.len() {
        match input[start..].iter().position(|&b| b == b'\n') {
            Some(offset) => {
                let mut end = start + offset;
                if end > start && input[end - 1] == b'\r' {
                    end -= 1;
                }
                list.push(truncate_at_nul(&input[start..end]));
                start += offset + 1;
            }
            None => {
                list.push(truncate_at_nul(&input[start..]));
                break;
            }
        }
    }
    list
}

/// `string_list_append(&list, sb.buf)`: the item is a C string, so a line
/// carrying an embedded NUL is kept only up to it.
///
/// `git column` has no `-z`, so NUL-separated input arrives as one long line and
/// everything past the first separator is dropped — which is what stock prints.
fn truncate_at_nul(line: &[u8]) -> Vec<u8> {
    let end = line.iter().position(|&b| b == 0).unwrap_or(line.len());
    line[..end].to_vec()
}

/// Port of `print_columns`: default the options, then dispatch on the layout.
fn print_columns(list: &[Vec<u8>], colopts: u32, opts: &Options, out: &mut Vec<u8>) -> Result<()> {
    if list.is_empty() {
        return Ok(());
    }
    let indent = opts.indent.as_deref().unwrap_or("");
    let nl = opts.nl.as_deref().unwrap_or("\n");
    let width = if opts.width != 0 {
        opts.width
    } else {
        term_columns() - 1
    };

    // `column_active`: only an explicit "always" renders a table.
    if colopts & COL_ENABLE_MASK != COL_ENABLED {
        display_plain(list, "", "\n", out);
        return Ok(());
    }
    match colopts & COL_LAYOUT_MASK {
        COL_PLAIN => display_plain(list, indent, nl, out),
        COL_ROW | COL_COLUMN => display_table(list, colopts, indent, nl, width, opts.padding, out)?,
        mode => crate::git_fatal!("invalid layout mode {mode}"),
    }
    Ok(())
}

/// Port of `display_plain`: one cell per line, no alignment.
fn display_plain(list: &[Vec<u8>], indent: &str, nl: &str, out: &mut Vec<u8>) {
    for item in list {
        out.extend_from_slice(indent.as_bytes());
        out.extend_from_slice(item);
        out.extend_from_slice(nl.as_bytes());
    }
}

/// The mutable state `display_table` threads through `layout`,
/// `compute_column_width`, `shrink_columns` and `display_cell`.
struct Table<'a> {
    /// Display width of every cell, indexed linearly.
    len: &'a [i64],
    /// Whether cells are filled down columns (`COL_COLUMN`) or across rows.
    by_column: bool,
    rows: i64,
    cols: i64,
    /// Index of the widest cell of each column; only populated for `dense`.
    width: Option<Vec<usize>>,
}

impl Table<'_> {
    /// Port of the `XY2LINEAR` macro.
    fn linear(&self, x: i64, y: i64) -> i64 {
        if self.by_column {
            x * self.rows + y
        } else {
            y * self.cols + x
        }
    }

    /// Port of `compute_column_width`.
    fn compute_column_width(&mut self) {
        let nr = self.len.len() as i64;
        let mut width = vec![0usize; self.cols.max(0) as usize];
        for x in 0..self.cols {
            // Upstream seeds this with `XY2LINEAR(x, 0)`, which the `cols`
            // recurrence keeps in range for every column that is actually
            // rendered; clamp so an unrendered trailing column cannot index
            // out of bounds the way the C code reads past its array.
            let seed = self.linear(x, 0).clamp(0, (nr - 1).max(0)) as usize;
            width[x as usize] = seed;
            for y in 0..self.rows {
                let i = self.linear(x, y);
                if i < nr && self.len[width[x as usize]] < self.len[i as usize] {
                    width[x as usize] = i as usize;
                }
            }
        }
        self.width = Some(width);
    }

    /// Port of `shrink_columns`: trade a row for more columns until the widest
    /// cell of every column no longer fits in the terminal width.
    fn shrink_columns(&mut self, indent_len: i64, padding: i64, total: i64) {
        let nr = self.len.len() as i64;
        while self.rows > 1 {
            let (rows, cols) = (self.rows, self.cols);
            self.rows -= 1;
            self.cols = div_round_up(nr, self.rows);
            self.compute_column_width();

            let widths = self.width.as_ref().expect("just computed");
            let mut total_width = indent_len;
            for &w in widths.iter().take(self.cols as usize) {
                total_width += self.len[w];
                total_width += padding;
            }
            if total_width > total {
                self.rows = rows;
                self.cols = cols;
                break;
            }
        }
        self.compute_column_width();
    }
}

/// Port of `display_table` and `display_cell`.
fn display_table(
    list: &[Vec<u8>],
    colopts: u32,
    indent: &str,
    nl: &str,
    total_width: i64,
    padding: i64,
    out: &mut Vec<u8>,
) -> Result<()> {
    let nr = list.len() as i64;
    let len: Vec<i64> = list.iter().map(|s| item_length(s)).collect();

    // `layout()`: size a table of equal cells.
    let mut initial_width = len.iter().copied().max().unwrap_or(0) + padding;
    if initial_width == 0 {
        // Upstream divides by this unconditionally; a zero divisor can only
        // arise from all-empty input with `--padding=0`, where git crashes.
        crate::git_fatal!("cannot lay out empty cells with --padding=0");
    }
    let mut cols = (total_width - indent.len() as i64) / initial_width;
    if cols == 0 {
        cols = 1;
    }
    let mut table = Table {
        len: &len,
        by_column: colopts & COL_LAYOUT_MASK == COL_COLUMN,
        rows: div_round_up(nr, cols),
        cols,
        width: None,
    };

    if colopts & COL_DENSE != 0 {
        table.shrink_columns(indent.len() as i64, padding, total_width);
    }
    // `initial_width` is the length of the shared blank-padding cell; it is
    // read as `empty_cell + len`, so it is only ever a source of spaces.
    initial_width = initial_width.max(0);
    let empty_cell = vec![b' '; initial_width as usize];

    for y in 0..table.rows {
        for x in 0..table.cols {
            let i = table.linear(x, y);
            if i >= nr || i < 0 {
                break;
            }
            let i = i as usize;

            let mut cell_len = table.len[i];
            if let Some(widths) = &table.width {
                let column_max = table.len[widths[x as usize]];
                if column_max < initial_width {
                    // The blank cell is `initial_width` wide; when this column
                    // is narrower, consume more of it so less is emitted.
                    cell_len += initial_width - column_max;
                    cell_len -= padding;
                }
            }

            let newline = if table.by_column {
                i as i64 + table.rows >= nr
            } else {
                x == table.cols - 1 || i as i64 == nr - 1
            };

            if x == 0 {
                out.extend_from_slice(indent.as_bytes());
            }
            out.extend_from_slice(&list[i]);
            if newline {
                out.extend_from_slice(nl.as_bytes());
            } else {
                let skip = cell_len.clamp(0, initial_width) as usize;
                out.extend_from_slice(&empty_cell[skip..]);
            }
        }
    }
    Ok(())
}

/// git's `DIV_ROUND_UP`, including its truncation behaviour on negatives.
fn div_round_up(a: i64, b: i64) -> i64 {
    (a + b - 1) / b
}

/// git's `item_length`, i.e. `utf8_strnwidth(s, strlen(s), 1)`: display width
/// with SGR escape sequences skipped.
///
/// ```c
/// glyph_width = utf8_width(&string, NULL);
/// if (glyph_width > 0)
///         width += glyph_width;
/// ```
///
/// (utf8.c:224-227.) A glyph `git_wcwidth()` scores at or below zero adds
/// nothing — which covers NUL (0) and every control character, `if (ch < 32 ||
/// (ch >= 0x7f && ch < 0xa0)) return -1` (utf8.c:96-97). A tab in a cell
/// therefore costs no columns at all.
///
/// Everything else counts as one column rather than `git_wcwidth()`'s answer —
/// see the module header. Bytes that are not valid UTF-8 count as one column
/// each, matching git's fallback for undecodable input.
fn item_length(s: &[u8]) -> i64 {
    let mut width = 0i64;
    let mut i = 0;
    while i < s.len() {
        let skip = esc_sequence_len(&s[i..]);
        if skip != 0 {
            i += skip;
            continue;
        }
        // Decode one UTF-8 scalar, or consume a single invalid byte.
        let take = utf8_len(s[i]).min(s.len() - i).max(1);
        let scalar = std::str::from_utf8(&s[i..i + take])
            .ok()
            .and_then(|text| text.chars().next());
        i += match scalar {
            Some(_) => take,
            None => 1,
        };
        let zero_or_less = scalar.is_some_and(|c| {
            let ch = c as u32;
            ch < 32 || (0x7f..0xa0).contains(&ch)
        });
        if !zero_or_less {
            width += 1;
        }
    }
    width
}

/// Port of `display_mode_esc_sequence_len`: the length of a leading
/// `ESC [ [0-9;]* m` sequence, or 0 when `s` does not start with one.
fn esc_sequence_len(s: &[u8]) -> usize {
    if s.first() != Some(&0x1b) || s.get(1) != Some(&b'[') {
        return 0;
    }
    let mut i = 2;
    while matches!(s.get(i), Some(b) if b.is_ascii_digit() || *b == b';') {
        i += 1;
    }
    if s.get(i) == Some(&b'm') {
        i + 1
    } else {
        0
    }
}

/// Byte length of the UTF-8 sequence a leading byte introduces.
fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

// ---------------------------------------------------------------------------
// Reusable layout API for the embedding commands (`git tag`/`branch`/`status`).
//
// These wrap the same engine `git column` uses, so a `--column[=<opts>]` on tag,
// branch or status lays its list out byte-identically to piping that list through
// `git column`. Nothing here changes `git column`'s own output — it only exposes
// the already-ported `git_column_config`, `parseopt_column_callback`,
// `finalize_colopts`, `column_active`, `explicitly_enable_column` and
// `print_columns` primitives under names the porcelain modules can call.
// ---------------------------------------------------------------------------

/// Column layout parameters for an embedding command, mirroring git's
/// `struct column_options`; `width == 0` means "ask the terminal".
pub(crate) struct ColumnOptions {
    pub width: i64,
    pub padding: i64,
    pub indent: Option<String>,
    pub nl: Option<String>,
}

/// Port of `git_column_config`: fold `column.ui` and `column.<command>` into
/// `colopts`. Returns git's config-error text (`error: … \nfatal: …`) on a bad
/// value, so the caller can print it verbatim and exit 128.
pub(crate) fn config_colopts(colopts: &mut u32, command: &str) -> Result<(), String> {
    apply_config(colopts, Some(command))
}

/// [`config_colopts`] keyed by the full `column.<command>` variable instead of the
/// bare command name, so the configuration this reads is spelled at the call site
/// rather than assembled here. `key` is looked up verbatim, after `column.ui`.
pub(crate) fn config_colopts_key(colopts: &mut u32, key: &str) -> Result<(), String> {
    apply_config_keys(colopts, &[COLUMN_UI.to_string(), key.to_string()])
}

/// `column_config()` (column.c:318-326) reduced to its check: whether `value` is a
/// token list git can read, and the `error()` text it prints first when it is not.
///
/// Used by the config callbacks that reach `git_column_config` before the command
/// runs (`crate::status_config`), which need to validate a value in place and
/// build their own origin-naming `fatal:` rather than have this module print one.
pub(crate) fn validate_config_value(value: &str) -> Result<(), String> {
    let mut colopts = 0u32;
    parse_config(&mut colopts, value)
}

/// Port of `parseopt_column_callback`: apply one `--column[=<opts>]` / `--no-column`
/// occurrence. `unset` is the `--no-column` form (git's "never"). A bad `<opts>`
/// token yields git's `unsupported option '<tok>'` text.
pub(crate) fn parseopt_column(
    colopts: &mut u32,
    arg: Option<&str>,
    unset: bool,
) -> Result<(), String> {
    *colopts |= COL_PARSEOPT;
    *colopts &= !COL_ENABLE_MASK;
    if unset {
        return Ok(());
    }
    *colopts |= COL_ENABLED;
    if let Some(a) = arg {
        parse_config(colopts, a)?;
    }
    Ok(())
}

/// Port of `finalize_colopts(colopts, -1)`: resolve `auto` against the real
/// terminal state (see [`finalize_colopts`]).
pub(crate) fn finalize(colopts: &mut u32) {
    finalize_colopts(colopts);
}

/// Port of the inline `column_active`: only an explicit "always" renders a table.
pub(crate) fn active(colopts: u32) -> bool {
    colopts & COL_ENABLE_MASK == COL_ENABLED
}

/// Port of the `explicitly_enable_column` macro: a `--column` on the command line
/// (`COL_PARSEOPT`) that resolves to active. Used for the `--column` vs `-n` /
/// `--verbose` conflict, which only dies when the user asked for columns explicitly.
pub(crate) fn explicitly_enabled(colopts: u32) -> bool {
    colopts & COL_PARSEOPT != 0 && active(colopts)
}

/// The value git resets `colopts` to when a conflicting option silently disables
/// columns (`colopts = 0`).
pub(crate) const DISABLED: u32 = COL_DISABLED;

/// Port of `builtin/clean.c`'s
/// `colopts = (colopts & ~COL_ENABLE_MASK) | COL_ENABLED`, whose comment reads
/// "always enable column display, we only consult column.* about layout strategy
/// and stuff". `column.clean=never` therefore does *not* turn the table off —
/// but `column.clean=plain` still switches to one item per line, and
/// `column.clean=column` still fills down columns instead of across rows.
pub(crate) fn force_enabled(colopts: u32) -> u32 {
    (colopts & !COL_ENABLE_MASK) | COL_ENABLED
}

/// `COL_ENABLED | COL_ROW`, the fixed layout `pretty_print_menus()` builds in a
/// `local_colopts` of its own — the interactive menus ignore `column.*` entirely.
pub(crate) const ENABLED_ROW: u32 = COL_ENABLED | COL_ROW;

/// Lay out `list` per `colopts`/`opts` (git's `print_columns`) and return the
/// bytes. An empty list yields empty output, matching git's early return.
pub(crate) fn layout(list: &[Vec<u8>], colopts: u32, opts: &ColumnOptions) -> Vec<u8> {
    let inner = Options {
        width: opts.width,
        padding: opts.padding,
        indent: opts.indent.clone(),
        nl: opts.nl.clone(),
    };
    let mut out = Vec::new();
    // print_columns only bails on the all-empty-cells + `--padding=0` corner, which
    // none of these callers reach (tag padding=2, branch/status padding=1).
    let _ = print_columns(list, colopts, &inner, &mut out);
    out
}


