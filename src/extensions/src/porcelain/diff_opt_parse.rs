//! `diff_opt_parse()` (diff.c:6303-6321) as `setup_revisions()` drives it: one
//! argv word at a time, against the `add_diff_options()` table, for a command that
//! does not render the diff itself and only needs to know what the options *did*.
//!
//! `handle_revision_opt()` hands every option it does not own to this parser
//! (revision.c:2758-2762), so a history walker accepts the whole diff option set
//! whether or not it ever produces a diff. `rev-list` is the case in point: it
//! takes `-S <string>` — consuming the needle as the option's value — and only
//! afterwards refuses the diff that option asked for:
//!
//! ```text
//! $ git rev-list -S edited HEAD
//! usage: git rev-list [<options>] <commit>... [--] [<path>...]
//! $ git rev-list -M HEAD
//! 3d82c9af02521829979abbf011a9b36bf0baa9b1
//! ```
//!
//! The parse-options flags decide the shape of every answer here:
//!
//! * `PARSE_OPT_ONE_SHOT` — one word (plus a detached value) per call; the return
//!   value is how many words were taken, and `0` means the word is not a diff
//!   option at all.
//! * `PARSE_OPT_KEEP_UNKNOWN_OPT` — an unknown option is handed back rather than
//!   refused, and abbreviations are off (`register_abbrev()` returns early,
//!   parse-options.c:502), so `--stat-w=3` is unknown, not `--stat-width=3`.
//! * `PARSE_OPT_NO_INTERNAL_HELP` — `-h` is unknown like any other letter.
//!
//! An error from `get_value()` or from an option callback is `PARSE_OPT_ERROR`,
//! which `parse_options()` answers with a bare `exit(129)` at that argv position;
//! a `die()` inside a callback (`--dirstat`, `--ignore-submodules`) is 128.

use super::{Arg, LongOpt};
use crate::parseopt::OptName;
use std::process::ExitCode;

/// `DIFF_FORMAT_*` (diff.h:100-116): only the bits the options below set.
const RAW: u32 = 0x0001;
const DIFFSTAT: u32 = 0x0002;
const NUMSTAT: u32 = 0x0004;
const SUMMARY: u32 = 0x0008;
const PATCH: u32 = 0x0010;
const SHORTSTAT: u32 = 0x0020;
const DIRSTAT: u32 = 0x0040;
const NAME: u32 = 0x0100;
const NAME_STATUS: u32 = 0x0200;
const CHECKDIFF: u32 = 0x0400;
const NO_OUTPUT: u32 = 0x0800;

/// `DIFF_PICKAXE_*` (diff.h:618-632).
const PICKAXE_ALL: u32 = 1;
const PICKAXE_REGEX: u32 = 2;
const PICKAXE_KIND_S: u32 = 4;
const PICKAXE_KIND_G: u32 = 8;
const PICKAXE_KIND_OBJFIND: u32 = 16;
const PICKAXE_KINDS_MASK: u32 = PICKAXE_KIND_S | PICKAXE_KIND_G | PICKAXE_KIND_OBJFIND;
const PICKAXE_KINDS_G_REGEX_MASK: u32 = PICKAXE_KIND_G | PICKAXE_REGEX;
const PICKAXE_KINDS_ALL_OBJFIND_MASK: u32 = PICKAXE_ALL | PICKAXE_KIND_OBJFIND;

/// The short half of `add_diff_options()` (diff.c:6022-6292): the letter, what it
/// does with a value, and the long name its callback answers to (`-u` is the
/// `--patch` bit operation without a long name of its own; `-z`, `-l`, `-R`, `-S`,
/// `-G` and `-O` have none at all and are named by their letter).
const SHORT_OPTS: &[(char, Arg, &str)] = &[
    ('p', Arg::None, "patch"),
    ('s', Arg::None, "no-patch"),
    ('u', Arg::None, "patch"),
    ('U', Arg::Optional, "unified"),
    ('W', Arg::None, "function-context"),
    ('X', Arg::Optional, "dirstat"),
    ('z', Arg::None, "-z"),
    ('B', Arg::Optional, "break-rewrites"),
    ('M', Arg::Optional, "find-renames"),
    ('D', Arg::None, "irreversible-delete"),
    ('C', Arg::Optional, "find-copies"),
    ('l', Arg::Required, "-l"),
    ('w', Arg::None, "ignore-all-space"),
    ('b', Arg::None, "ignore-space-change"),
    ('I', Arg::Required, "ignore-matching-lines"),
    ('a', Arg::None, "text"),
    ('R', Arg::None, "-R"),
    ('S', Arg::Required, "-S"),
    ('G', Arg::Required, "-G"),
    ('O', Arg::Required, "-O"),
];

/// What the options seen so far did to `struct diff_options`, reduced to the
/// fields a walker that prints no diff can observe.
#[derive(Default)]
pub(super) struct DiffOpts {
    /// `options->output_format`.
    output_format: u32,
    /// `options->pickaxe_opts`.
    pickaxe_opts: u32,
    /// `options->filter` / `options->filter_not`.
    filter: super::diff_filter::Filter,
    /// `options->flags.follow_renames`.
    follow_renames: bool,
    /// `options->flags.quick` — `--quiet`.
    pub(super) quick: bool,
    /// `options->max_depth`, when `--max-depth` parsed it.
    max_depth: Option<i32>,
    /// `options->line_prefix` was set: `graph.c` writes it ahead of every record
    /// whether or not a graph is drawn.
    line_prefix: bool,
    /// `options->file` was redirected by `--output`; only the graph writes there.
    output: bool,
    /// `options->use_color` as an option left it — `None` while no option touched
    /// it, which is the plain output this walker draws.
    use_color: Option<ColorBool>,
}

impl DiffOpts {
    /// Whether an option was given whose effect on the walker's own output this
    /// port does not draw: the line prefix, a graph sent to `--output`, and colour
    /// where it is visible — the graph and the `--pretty`/`--format` body that
    /// `pretty_print_commit()` renders with `ctx.color` (builtin/rev-list.c:338).
    /// The caller refuses the command rather than print what stock would not.
    pub(super) fn unported(&self, graph: bool, verbose_header: bool) -> bool {
        self.line_prefix || (self.output && graph) || (self.wants_color() && (graph || verbose_header))
    }

    /// `want_color(options->use_color)` for a value an option set: `auto` asks
    /// whether stdout is a terminal (`check_auto_color()`, color.c).
    fn wants_color(&self) -> bool {
        match self.use_color {
            Some(ColorBool::Always) => true,
            Some(ColorBool::Auto) => std::io::IsTerminal::is_terminal(&std::io::stdout()),
            Some(ColorBool::Never) | None => false,
        }
    }

    /// `revs->diff` as `setup_revisions()` derives it (revision.c:3182-3190):
    /// any output format but `NO_OUTPUT`, a pickaxe, a `--diff-filter`, or
    /// `--follow` means the caller asked for a diff.
    pub(super) fn wants_diff(&self) -> bool {
        self.output_format & !NO_OUTPUT != 0
            || self.pickaxe_opts & PICKAXE_KINDS_MASK != 0
            || self.filter.given()
            || self.follow_renames
    }

    /// The `die()`s of `diff_setup_done()` (diff.c:5223-5357) that depend on nothing
    /// but the parsed options and the pathspec, in their order. `pathspecs` is
    /// `revs->diffopt.pathspec`: the prune data, unless `--full-diff` left it empty
    /// (revision.c:3200-3206).
    pub(super) fn setup_done<S: AsRef<[u8]>>(&self, pathspecs: &[S]) -> Result<(), String> {
        let check_mask = NAME | NAME_STATUS | CHECKDIFF | NO_OUTPUT;
        if (self.output_format & check_mask).count_ones() > 1 {
            return Err("options '--name-only', '--name-status', '--check', and '-s' cannot be used together".into());
        }
        if (self.pickaxe_opts & PICKAXE_KINDS_MASK).count_ones() > 1 {
            return Err("options '-G', '-S', and '--find-object' cannot be used together".into());
        }
        if (self.pickaxe_opts & PICKAXE_KINDS_G_REGEX_MASK).count_ones() > 1 {
            return Err("options '-G' and '--pickaxe-regex' cannot be used together, use '--pickaxe-regex' with '-S'".into());
        }
        if (self.pickaxe_opts & PICKAXE_KINDS_ALL_OBJFIND_MASK).count_ones() > 1 {
            return Err("options '--pickaxe-all' and '--find-object' cannot be used together, use '--pickaxe-all' with '-G' and '-S'".into());
        }
        // `diff_check_follow_pathspec(&options->pathspec, 1)` (diff.c:5196-5218).
        if self.follow_renames {
            if pathspecs.len() != 1 {
                return Err("--follow requires exactly one pathspec".into());
            }
            if let Some(magic) = super::whatchanged::unsupported_follow_magic(&String::from_utf8_lossy(pathspecs[0].as_ref())) {
                return Err(format!("pathspec magic not supported by --follow: '{magic}'"));
            }
        }
        // `if (options->pathspec.has_wildcard && options->max_depth_valid)`
        // (diff.c:5355-5356); `max_depth_valid` is `max_depth >= 0`.
        if self.max_depth.is_some_and(|d| d >= 0) && pathspecs.iter().any(|s| has_wildcard(s.as_ref())) {
            return Err("max-depth cannot be used with wildcard pathspecs".into());
        }
        Ok(())
    }
}

/// `enum git_colorbool` (color.h) for the values an option can store.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorBool {
    Never,
    Always,
    Auto,
}

/// What one call made of the word it was handed.
pub(super) enum Step {
    /// Not a diff option: `diff_opt_parse()` returned 0.
    Unknown,
    /// Claimed, taking this many argv words (the option and a detached value).
    Took(usize),
    /// Refused, with the diagnostic already on stderr.
    Exit(ExitCode),
}

/// One `diff_opt_parse()` call on `args[0]`. `args` ends where the caller's argv
/// does for parse-options — at the `--` separator `setup_revisions()` cut off
/// before its option loop (revision.c:3015-3033) — so a detached value is never
/// taken from behind it.
pub(super) fn diff_opt_parse(repo: &gix::Repository, args: &[String], st: &mut DiffOpts) -> Step {
    let arg = args[0].as_str();
    let Some(body) = arg.strip_prefix('-') else {
        return Step::Unknown;
    };
    if body.is_empty() {
        return Step::Unknown;
    }
    let next = args.get(1).map(String::as_str);
    match body.strip_prefix('-') {
        // `--` and `--end-of-options` stop the parse without taking anything.
        Some("") | Some("end-of-options") => Step::Unknown,
        Some(long) => parse_long(repo, long, next, st),
        None => parse_short(repo, body, next, st),
    }
}

/// `parse_short_opt()` over a cluster (parse-options.c:1062-1111). Every letter is
/// applied as it is read; an unknown one ends the cluster as unknown, *after* the
/// letters in front of it took effect — git cannot undo them either.
///
/// `check_typos()` runs over the whole word (minus its dash) once: when the first
/// letter is unknown, and after the first letter when the cluster goes on — so
/// `-raw` and `-stat` are refused as long options typed with one dash, while
/// `-Sedited` (whose `S` took the rest as its value) never reaches it.
fn parse_short(repo: &gix::Repository, cluster: &str, next: Option<&str>, st: &mut DiffOpts) -> Step {
    let typos = || crate::parseopt::check_typos(cluster, super::diff_pairs::LONG_OPTS).map_err(ExitCode::from);
    let mut rest = cluster;
    let mut first = true;
    while let Some(c) = rest.chars().next() {
        let Some(&(_, kind, name)) = SHORT_OPTS.iter().find(|(s, _, _)| *s == c) else {
            if first {
                if let Err(code) = typos() {
                    return Step::Exit(code);
                }
            }
            return Step::Unknown;
        };
        rest = &rest[c.len_utf8()..];
        let optname = OptName::Short(c);
        let (value, took) = match kind {
            Arg::None => (None, 1),
            Arg::Optional => (Some(rest).filter(|v| !v.is_empty()), 1),
            Arg::Required | Arg::LastArg => match (rest.is_empty(), next) {
                (false, _) => (Some(rest), 1),
                (true, Some(v)) => (Some(v), 2),
                (true, None) => return Step::Exit(crate::parseopt::requires_value(optname)),
            },
        };
        if let Err(code) = apply(repo, name, false, value, st) {
            return Step::Exit(code);
        }
        // A letter that takes a value, attached or not, ends the cluster.
        if !matches!(kind, Arg::None) {
            return Step::Took(took);
        }
        if first && !rest.is_empty() {
            if let Err(code) = typos() {
                return Step::Exit(code);
            }
        }
        first = false;
    }
    Step::Took(1)
}

/// `parse_long_opt()` (parse-options.c:519-594) without its abbreviation half,
/// which `PARSE_OPT_KEEP_UNKNOWN_OPT` turns off.
fn parse_long(repo: &gix::Repository, arg: &str, next: Option<&str>, st: &mut DiffOpts) -> Step {
    let mut arg_start = arg;
    let mut flags_unset = false;
    let mut no_no = false;
    if let Some(rest) = arg_start.strip_prefix("no-") {
        arg_start = rest;
        match rest.strip_prefix("no-") {
            Some(rest2) => {
                arg_start = rest2;
                no_no = true;
            }
            None => flags_unset = true,
        }
    }
    for opt in super::diff_pairs::LONG_OPTS {
        let (long_name, opt_unset) = match opt.name.strip_prefix("no-") {
            Some(stem) => (stem, true),
            None if no_no => continue,
            None => (opt.name, false),
        };
        let unset = flags_unset ^ opt_unset;
        if unset && !opt.neg {
            continue;
        }
        let Some(rest) = arg_start.strip_prefix(long_name) else {
            continue;
        };
        let attached = match rest.strip_prefix('=') {
            Some(v) => Some(v),
            None if rest.is_empty() => None,
            None => continue,
        };
        return get_value(repo, opt, unset, attached, next, st);
    }
    Step::Unknown
}

/// `do_get_value()`'s checks (parse-options.c:130-143) and `get_arg()`
/// (parse-options.c:47-62) for one resolved long option, then its effect.
fn get_value(
    repo: &gix::Repository,
    opt: &LongOpt,
    unset: bool,
    attached: Option<&str>,
    next: Option<&str>,
    st: &mut DiffOpts,
) -> Step {
    // `optname()`: the table's own spelling, with `no-` glued on for the unset sense.
    let optname = match unset {
        true => OptName::Unset(opt.name),
        false => OptName::Long(opt.name),
    };
    if unset && attached.is_some() {
        return Step::Exit(crate::parseopt::takes_no_value(optname));
    }
    let (value, took) = match (unset, opt.arg) {
        (true, _) => (None, 1),
        (false, Arg::None) if attached.is_some() => {
            return Step::Exit(crate::parseopt::takes_no_value(optname))
        }
        (false, Arg::None) | (false, Arg::Optional) => (attached, 1),
        (false, Arg::Required | Arg::LastArg) => match (attached, next) {
            (Some(v), _) => (Some(v), 1),
            (None, Some(v)) => (Some(v), 2),
            (None, None) => return Step::Exit(crate::parseopt::requires_value(optname)),
        },
    };
    match apply(repo, opt.name, unset, value, st) {
        Ok(()) => Step::Took(took),
        Err(code) => Step::Exit(code),
    }
}

/// Print a callback's `error()` and answer parse-options' 129.
fn error(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::from(crate::parseopt::USAGE_ERROR)
}

/// `options->output_format &= ~NO_OUTPUT; |= bits` — what `OPT_BITOP` with
/// `NO_OUTPUT` as its mask, `enable_patch_output()` and the stat/dirstat callbacks
/// all do.
fn set_format(st: &mut DiffOpts, bits: u32) {
    st.output_format = (st.output_format & !NO_OUTPUT) | bits;
}

/// The effect of one table entry, by the name its callback answers to. An entry
/// spelled `no-<x>` in the table keeps that spelling here, `unset` being its
/// `OPT_UNSET` sense.
fn apply(
    repo: &gix::Repository,
    name: &str,
    unset: bool,
    value: Option<&str>,
    st: &mut DiffOpts,
) -> Result<(), ExitCode> {
    // The value checks every diff command shares — `diff_optval::reject` is fed
    // the long spelling, which is how each callback names itself.
    let spelled = match value {
        Some(v) => format!("--{name}={v}"),
        None => format!("--{name}"),
    };
    let reject = |s: &str| {
        super::diff_optval::reject(s).map(|line| {
            eprintln!("{line}");
            ExitCode::from(crate::parseopt::USAGE_ERROR)
        })
    };
    match name {
        "patch" => set_format(st, PATCH),
        // `OPT_SET_INT('s', "no-patch", …, DIFF_FORMAT_NO_OUTPUT)`: the unset sense
        // (`--no-no-patch`) stores 0.
        "no-patch" => st.output_format = if unset { 0 } else { NO_OUTPUT },
        "unified" => {
            if let Some(code) = reject(&spelled) {
                return Err(code);
            }
            set_format(st, PATCH);
        }
        "raw" => set_format(st, RAW),
        "patch-with-raw" => set_format(st, PATCH | RAW),
        "patch-with-stat" => set_format(st, PATCH | DIFFSTAT),
        "numstat" => set_format(st, NUMSTAT),
        "shortstat" => set_format(st, SHORTSTAT),
        "summary" => set_format(st, SUMMARY),
        // `diff_opt_dirstat()` (diff.c:5685-5698): `--dirstat-by-file` parses
        // `files` first, `--cumulative` is `--dirstat=cumulative`; a bad parameter
        // is `parse_dirstat_opt()`'s `die()`.
        "dirstat" | "dirstat-by-file" | "cumulative" => {
            let probe = match (name, value) {
                (_, None) => None,
                ("dirstat-by-file", Some(v)) => Some(format!("--dirstat-by-file={v}")),
                (_, Some(v)) => Some(format!("--dirstat={v}")),
            };
            if let Some(text) = probe.as_deref().and_then(super::diff_optval::dirstat_reject) {
                eprint!("{text}");
                return Err(ExitCode::from(crate::parseopt::FATAL));
            }
            set_format(st, DIRSTAT);
        }
        // `OPT_BIT_F`: or'ed in, `NO_OUTPUT` left alone.
        "check" => st.output_format |= CHECKDIFF,
        "name-only" => st.output_format |= NAME,
        "name-status" => st.output_format |= NAME_STATUS,
        "stat" | "stat-width" | "stat-name-width" | "stat-graph-width" | "stat-count" => {
            if let Some(code) = reject(&spelled) {
                return Err(code);
            }
            set_format(st, DIFFSTAT);
        }
        "compact-summary" => {
            if !unset {
                set_format(st, DIFFSTAT);
            }
        }
        "binary" => set_format(st, PATCH),
        // `OPT_COLOR_FLAG` → `parse_opt_color_flag_cb()` (parse-options-cb.c:50-63):
        // bare is `always`, the negation `never`, a value one of the three words.
        "color" => {
            if let Some(code) = reject(&spelled) {
                return Err(code);
            }
            st.use_color = Some(match (unset, value.map(str::to_ascii_lowercase).as_deref()) {
                (true, _) | (false, Some("never")) => ColorBool::Never,
                (false, Some("auto")) => ColorBool::Auto,
                _ => ColorBool::Always,
            });
        }
        "ws-error-highlight" => {
            if let Err(message) = crate::diffopt::check(name, value) {
                return Err(error(&message));
            }
        }
        "line-prefix" => st.line_prefix = true,
        // `diff_opt_output()` (diff.c:5800-5815): the file is opened — created or
        // truncated — while the option is parsed, and colour is forced off unless
        // it was already `always`.
        "output" => {
            super::diff::open_output_file(value.unwrap_or_default())?;
            st.output = true;
            if st.use_color != Some(ColorBool::Always) {
                st.use_color = Some(ColorBool::Never);
            }
        }
        "inter-hunk-context" | "output-indicator-new" | "output-indicator-old"
        | "output-indicator-context" | "break-rewrites" | "find-renames" | "find-copies"
        | "diff-algorithm" | "word-diff" | "submodule" => {
            if let Some(code) = reject(&spelled) {
                return Err(code);
            }
            // `--word-diff=color` sets `use_color = GIT_COLOR_ALWAYS`.
            if name == "word-diff" && value == Some("color") {
                st.use_color = Some(ColorBool::Always);
            }
        }
        // `diff_opt_color_words()` forces `GIT_COLOR_ALWAYS` (diff.c:5624-5634).
        "color-words" => st.use_color = Some(ColorBool::Always),
        "color-moved" | "color-moved-ws" => {
            let flag = match unset {
                true => format!("--no-{name}"),
                false => spelled.clone(),
            };
            let mut ignored = super::diff_color::MoveWordOpts::default();
            if let Some(Err(text)) = ignored.parse_flag(&flag, &mut None) {
                eprintln!("{text}");
                return Err(ExitCode::from(crate::parseopt::USAGE_ERROR));
            }
        }
        "follow" => st.follow_renames = !unset,
        // `OPT_INTEGER('l', NULL, &options->rename_limit, …)`.
        "-l" => {
            let v = value.unwrap_or_default();
            if let Err(e) = crate::optint::integer(&crate::optint::short_opt('l'), v) {
                return Err(error(e.message()));
            }
        }
        // `diff_opt_ignore_regex()` (diff.c:5838-5856) carries `BUG_ON_OPT_NEG`, but
        // its table entry has no `PARSE_OPT_NONEG`, so the negation reaches the
        // callback and git aborts.
        "ignore-matching-lines" => match value {
            None => {
                eprintln!("BUG: diff.c:5844: option callback does not expect negation");
                return Err(ExitCode::from(134));
            }
            Some(v) => {
                if super::diff_pickaxe::compile_regex(v.as_bytes()).is_err() {
                    return Err(error(&format!("invalid regex given to -I: '{v}'")));
                }
            }
        },
        // `diff_opt_ignore_submodules()` → `handle_ignore_submodules_arg()`
        // (submodule.c:429-449), whose refusal is a `die()`.
        "ignore-submodules" => {
            let v = value.unwrap_or("all");
            if !matches!(v, "all" | "untracked" | "dirty" | "none") {
                eprintln!("fatal: bad --ignore-submodules argument: {v}");
                return Err(ExitCode::from(crate::parseopt::FATAL));
            }
        }
        // `diff_opt_pickaxe_string()` / `diff_opt_pickaxe_regex()`
        // (diff.c:5858-5882): the kind bit is set before the empty-needle refusal.
        "-S" | "-G" => {
            st.pickaxe_opts |= if name == "-S" { PICKAXE_KIND_S } else { PICKAXE_KIND_G };
            if value.is_some_and(str::is_empty) {
                eprintln!("{}", super::diff_optval::pickaxe_empty(name.as_bytes()[1]));
                return Err(ExitCode::from(crate::parseopt::USAGE_ERROR));
            }
        }
        "pickaxe-all" => st.pickaxe_opts |= PICKAXE_ALL,
        "pickaxe-regex" => st.pickaxe_opts |= PICKAXE_REGEX,
        "find-object" => match crate::objname::find_object(repo, value.unwrap_or_default()) {
            Ok(_) => st.pickaxe_opts |= PICKAXE_KIND_OBJFIND,
            Err(e) => return Err(e.report()),
        },
        "diff-filter" => {
            let v = value.unwrap_or_default();
            if let Err(bad) = st.filter.accumulate(v) {
                return Err(error(&format!("unknown change class '{bad}' in --diff-filter={v}")));
            }
        }
        "max-depth" => {
            let v = value.unwrap_or_default();
            match super::diff_pairs::parse_git_int(v) {
                Some(depth) => st.max_depth = Some(depth),
                None => return Err(error(&format!("invalid value for '--max-depth': '{v}'"))),
            }
        }
        "quiet" => st.quick = !unset,
        // Every other entry only moves a field the walk never reads: rename and
        // whitespace handling, prefixes, algorithms, `-O`, `--relative`, `-R`, `-z`
        // (the diff's own terminator, not rev-list's), `--exit-code` and the like.
        _ => {}
    }
    Ok(())
}

/// `pathspec->has_wildcard` for one element: `nowildcard_len < len`, where a
/// `:(literal)` element has no wildcard at all (pathspec.c:520-532).
fn has_wildcard(elt: &[u8]) -> bool {
    use gix::bstr::ByteSlice;
    let Ok(element) = crate::pathspec::parse_element_magic(elt.as_bstr()) else {
        return false;
    };
    if element.magic & crate::pathspec::MAGIC_LITERAL != 0 {
        return false;
    }
    elt[element.path_start..]
        .iter()
        .any(|b| matches!(b, b'*' | b'?' | b'[' | b'\\'))
}
