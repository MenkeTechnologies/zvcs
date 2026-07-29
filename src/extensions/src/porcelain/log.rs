use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::prelude::ObjectIdExt;
use gix::hash::ObjectId;
use gix::object::tree::diff::ChangeDetached;
use gix::objs::tree::EntryKind;

use super::filespec::{content_of, count_changed_lines, is_binary};
use super::line_log;

/// The terminal width git assumes for `--stat` when stdout is not a terminal.
/// git's `MINIMUM_ABBREV`: no `--abbrev` may cut an id shorter than this.
const MINIMUM_ABBREV: usize = 4;

/// git's `DEFAULT_ABBREV`, the length a valueless `--abbrev` selects.
const DEFAULT_ABBREV: usize = 7;

const STAT_TERM_WIDTH: usize = 80;

/// `--stat` width geometry, in git's `stat_width`/`stat_name_width`/`stat_graph_width`
/// encoding (`show_stats()` / `diff_opt_stat()`): `-1` == "unset" (the terminal width
/// for `width`, "uncapped" for the name/graph columns), seeded from the
/// `diff.statNameWidth`/`diff.statGraphWidth` config and then overridden by any explicit
/// `--stat*` flag (a `--stat-name-width=0` flag legitimately un-caps a positive config).
/// `count` is `0` == "all files", set by `--stat-count`/`--stat=,,<n>`.
#[derive(Clone, Copy)]
struct StatWidths {
    width: i64,
    name_width: i64,
    graph_width: i64,
    count: i64,
}

impl Default for StatWidths {
    fn default() -> Self {
        StatWidths {
            width: -1,
            name_width: -1,
            graph_width: -1,
            count: 0,
        }
    }
}

/// Parse an integer with git's lenient `strtoul`-ish behavior for a `--stat*=<n>`
/// value; a non-numeric value leaves the slot at its "unset" sentinel.
fn parse_stat_i64(s: &str) -> i64 {
    s.parse::<i64>().unwrap_or(-1)
}

/// Parse `--stat=<width>[,<name-width>[,<count>]]` (`diff_opt_stat()`): each present,
/// numeric field overwrites the corresponding slot; an empty or non-numeric field is
/// left unchanged, which is byte-equivalent to git's `strtoul` (empty == `0` == unset).
fn parse_stat_geometry(sw: &mut StatWidths, spec: &str) {
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

/// git's ref-decoration style (`--decorate` / `log.decorate`): whether commit
/// decorations are shown and, when shown, with short (`main`) or full
/// (`refs/heads/main`) ref names.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecorateStyle {
    /// No decorations on the built-in header/oneline formats.
    Off,
    /// `main`, `tag: v1`, `origin/main`.
    Short,
    /// `refs/heads/main`, `tag: refs/tags/v1`, `refs/remotes/origin/main`.
    Full,
}

/// git's `parse_decoration_style`: a maybe-bool (`true`/`false`/`yes`/`no`/
/// `on`/`off`/integer), or the words `short`/`full`/`auto`. `auto` resolves to
/// `Short` when stdout is a terminal and `Off` otherwise, matching git's
/// `auto_decoration_style`. Returns `None` for a value git rejects — config
/// treats that as `Off`, while `--decorate=<value>` makes it fatal.
pub(crate) fn parse_decoration_style(value: &str) -> Option<DecorateStyle> {
    let lower = value.to_ascii_lowercase();
    match lower.as_str() {
        "true" | "yes" | "on" | "short" => return Some(DecorateStyle::Short),
        "false" | "no" | "off" | "" => return Some(DecorateStyle::Off),
        "full" => return Some(DecorateStyle::Full),
        "auto" => {
            return Some(if std::io::stdout().is_terminal() {
                DecorateStyle::Short
            } else {
                DecorateStyle::Off
            })
        }
        _ => {}
    }
    // git falls back to integer parsing: a non-zero value is true (Short).
    if let Ok(n) = lower.parse::<i64>() {
        return Some(if n != 0 {
            DecorateStyle::Short
        } else {
            DecorateStyle::Off
        });
    }
    None
}

/// `git log` — commit history reachable from a starting revision (default `HEAD`).
///
/// Ported invocation forms:
///   * `git log [<rev>...]`                      → history from `HEAD`, a revision, or the
///     union of several revisions
///   * `-- <pathspec>...`                        → path-limited traversal: show only commits
///     that touched a matching plain pathspec (magic pathspecs surfaced terse)
///   * `-n N` / `--max-count=N` / `-N` / `-nN`   → limit the number of commits shown
///   * `--skip=N`                                → drop the first N selected commits
///   * `--all`                                   → start from every ref plus `HEAD`
///   * `--merges` / `--no-merges`                → keep only (or drop) multi-parent commits
///   * `--min-parents=N` / `--max-parents=N` and
///     their `--no-` forms                       → parent-count limiting
///   * `--first-parent`                          → follow only the first parent
///   * `--follow`                                 → track one path across renames
///   * `-m` / `-c` / `--cc` / `--diff-merges=<m>` → what a merge's diff shows
///   * `--reverse`                               → emit the selected commits oldest-first
///   * `--date-order` / `--topo-order`           → git's two topological sort orders
///   * `--oneline`, `--pretty=`/`--format=` with
///     `oneline`, `short`, `medium`, `full`, `fuller`, `raw`, `reference`, and
///     `format:`/`tformat:` strings (last flag wins; an invalid value is rejected
///     exactly as git's `get_commit_format` does). User-format placeholders include
///     `%C`/`%C(...)` colors (with `%C(auto)`), `%d`/`%D` ref decorations, and
///     `%cr`/`%ar` relative dates, alongside the hash/tree/parent/author/committer/
///     subject/body set
///   * `--abbrev-commit` / `--no-abbrev-commit`, `--parents`
///   * `--date=<mode>`                           → `default`/`short`/`iso`/`iso-strict`/
///     `rfc`/`unix`/`raw`/`relative` (the remaining zone-dependent modes `human`/`local`
///     are surfaced terse)
///   * `--color[=<when>]` / `--no-color`         → enable/disable the `%C` and
///     `%C(auto)`-gated decoration colors (`always`/`never`/`auto`; auto colors when
///     stdout is a terminal or a pager is in use)
///   * `--name-only`, `--name-status`, `--stat`,
///     `--numstat`, `--shortstat`                → per-commit diff against the first parent
///     (`--name-only`/`--name-status` are mutually exclusive and suppress the count
///     formats); `-s`/`--no-patch` accepted as no-ops
///   * `-q`/`--quiet` / `--no-quiet`               → git's position-independent
///     NO_OUTPUT: with no diff requested it changes nothing (`git log` shows no diff
///     by default), and any explicit `-p`/`--stat` still wins, so its only visible
///     effect is the `--name-only`/`--name-status` + NO_OUTPUT conflict
///   * `--decorate[=short|full|auto|no]` / `--no-decorate` → ref decorations on the
///     built-in header/oneline formats, defaulting to `log.decorate` and then to
///     `auto`. `--decorate-refs=<pattern>` and `--decorate-refs-exclude=<pattern>`
///     (both repeatable, matched with git's `normalize_glob_ref` +
///     `match_ref_pattern` rules) narrow which refs may decorate, and
///     `--clear-decorations` empties both lists and drops the default
///     known-namespace restriction, exposing refs such as `refs/bisect/*`.
///     `log.excludeDecoration` and `log.initialDecorationSet=all` are honored
///   * `--use-mailmap`/`--mailmap` and their `--no-` forms → resolve the
///     `Author:`/`Commit:` identities of the built-in header formats through
///     `.mailmap`, defaulting to `log.mailmap` (true, as in git since 2.24).
///     Like git, this affects only `pp_user_info`'s formats: `oneline`, `raw` and
///     user formats print the identity as the commit recorded it
///   * `--source` / `--no-source`                  → annotate each commit with the
///     ref/argument it was first reached from (`\t<source>` after the hash), on the
///     built-in header formats (not the user or `reference` formats), with git's
///     parent-inheritance during the walk
///   * `-p`/`--patch`/`-u`                        → per-commit `diff --git` patch against the
///     first parent (the empty tree for a root commit), three lines of context; suppressed by
///     `--name-only`/`--name-status`, emitted after the count formats otherwise, and skipped
///     for merge commits (git shows no diff there without `-m`/`-c`/`--cc`). Rendered by the
///     same pipeline as `git diff`, so the two produce byte-identical patches. The root
///     commit's empty-tree diff obeys `log.showRoot` (default true); `--root` forces it on.
///   * `--graph`                                 → git's ASCII commit graph (see below)
///   * `-L<start>,<end>:<file>` and its
///     `<start>,+<n>` / `<start>,-<n>` / `/<regex>/` / `:<funcname>` / `^:<funcname>`
///     spellings, repeatable across files and across ranges of one file → git's
///     line-level traversal (see [`super::line_log`]): only the commits that changed
///     a tracked line are shown, each with a diff clipped to that line range. `-L`
///     implies `--topo-order` and, with no other diff format given, `-p`; it is
///     rejected against a pathspec and against the count formats exactly as git
///     rejects them.
///
/// ### Rename detection in the per-commit diff — a measured gap
///
/// `git log`'s own diffs run `diffcore_rename` (porcelain defaults `diff.renames`
/// on), so a commit that renamed a file shows `R<score> <old> <new>` in
/// `--name-status`, `<old> => <new>` in `--stat`, and a `rename from`/`rename to`
/// patch header. The per-commit diff here does not run that pass, so such a commit
/// renders as a delete plus an add instead. Measured against stock git 2.50.1 on a
/// `git mv`-created commit; it is the same for `log -p`, `--name-status` and
/// `--stat`, with or without `--follow`. `git diff` is unaffected — it has the
/// pass, and `--follow`'s *commit list* is unaffected too, because that is decided
/// by the rename search below rather than by the diff.
///
/// `--follow` itself is ported: `try_to_follow_renames()`'s rewrite of the
/// pathspec, one commit at a time along the first parent, so the log walks back
/// through every name the file has had. Its exact-rename pass is git's; the
/// inexact one uses the same `diffcore_count_changes()` estimator, which agrees on
/// the score but may pick a different winner when several deletions in one commit
/// score alike.
///
/// Output separation follows git's `format:` (separator) versus `tformat:`
/// (terminator) distinction, which is why `--format=%s` and `--pretty=format:%s`
/// lay out differently; `--oneline`/`--pretty=oneline` are terminator formats.
///
/// Deviations, surfaced rather than faked:
///   * `--graph` renders commits with at most two parents. An octopus merge is
///     rejected instead of being drawn wrong.
///   * Rename detection is off, so a rename shows as a delete plus an add.
///   * `--stat` assumes an 80-column terminal (`COLUMNS` is not consulted) and measures
///     paths in `char`s; the `--stat-width`/`--stat=<w>`, `--stat-name-width`,
///     `--stat-graph-width`, and `--stat-count` flags and the
///     `diff.statNameWidth`/`diff.statGraphWidth` config are honored (flag over config
///     over the 80-column / uncapped default).
///   * Pathspec limiting is git's default history simplification: a commit
///     TREESAME to any parent over the pathspec is simplified away and the
///     history behind that parent alone is followed, so a merge that took one
///     side's change drops out along with the side it did not take. The diff
///     formats (`-p`, `--stat`, `--name-*`) are limited to the same paths.
///     `--full-history`/`--simplify-merges` are not implemented.
///   * Revision ranges are supported: `A..B` (`^A B`), `A...B` (symmetric
///     difference, excluding the merge-base), and a leading `^A` exclusion.
///   * `--grep`/`--author` filters and every flag not listed above are rejected.
pub fn log(args: &[String]) -> Result<ExitCode> {
    // Repeated object reads (one per rendered commit) re-inflate from the pack
    // without a cache; gix ships one and simply does not enable it by default.
    // A few MB turns `log` on a deep history from thousands of decompressions
    // into a warm-cache walk.
    let mut repo = gix::discover(".")?;
    // Rendering re-reads every walked commit for its message; without gix's
    // object cache each read re-inflates from the pack. Enabling it is the
    // difference between one decompression per commit and one per cache miss.
    repo.object_cache_size_if_unset(8 * 1024 * 1024);
    // gix's DEFAULT pack cache is a 64-entry linked list; git ships a 96MB
    // delta-base cache (core.deltaBaseCacheLimit). On a deep history every
    // rendered commit re-resolves its delta chain against those 64 slots, which
    // is where `log` spent its time. Size it like git.
    repo.objects.set_pack_cache(|| {
        Box::new(gix::odb::pack::cache::lru::MemoryCappedHashmap::new(96 * 1024 * 1024))
    });

    // Config supplies the defaults; the flags below override them. git reads
    // these in `git_log_config` before parsing args, and validates `log.date`
    // there — an invalid value is fatal even when `--date` later overrides it.
    let (cfg_abbrev_commit, cfg_date_mode, cfg_show_root, cfg_decorate, cfg_mailmap) = {
        let snap = repo.config_snapshot();
        let abbrev = snap.boolean("log.abbrevCommit").unwrap_or(false);
        // `log.mailmap` has defaulted to true since git 2.24, so the built-in
        // formats route identities through `.mailmap` unless `--no-use-mailmap`
        // or `log.mailmap=false` turns it off.
        let mailmap = snap.boolean("log.mailmap").unwrap_or(true);
        // `log.decorate` sets the default decoration style for the built-in
        // header/oneline formats. It reuses git's `parse_decoration_style`, so it
        // accepts a maybe-bool plus `short`/`full`/`auto`; an invalid value is
        // treated as `Off` (git's `decoration_style = 0`), never fatal. `None`
        // here means the key is absent, so the built-in default (`auto`) applies.
        let decorate: Option<DecorateStyle> = match snap.boolean("log.decorate") {
            Some(true) => Some(DecorateStyle::Short),
            Some(false) => Some(DecorateStyle::Off),
            None => snap.string("log.decorate").map(|v| {
                parse_decoration_style(&v.to_str_lossy()).unwrap_or(DecorateStyle::Off)
            }),
        };
        // `log.showRoot` defaults to true: the root commit is shown as a big
        // creation event (a diff against the empty tree). `--root` on the command
        // line forces it on but there is no `--no-root`, so config is the only way
        // to suppress the root diff.
        let show_root = snap.boolean("log.showRoot").unwrap_or(true);
        let date = match snap.string("log.date") {
            Some(v) => {
                let v = v.to_str_lossy();
                match parse_date_mode(&v) {
                    Some(m) => m,
                    None => {
                        eprintln!("fatal: unknown date format {v}");
                        return Ok(ExitCode::from(128));
                    }
                }
            }
            None => DateMode::Default,
        };
        (abbrev, date, show_root, decorate, mailmap)
    };

    // `--stat` width geometry, seeded from `diff.statNameWidth`/`diff.statGraphWidth`
    // (`git_diff_ui_config()`); a later `--stat*` flag overrides the corresponding slot.
    // git loads config before parsing args, so the flag always wins.
    let mut stat_widths = StatWidths::default();
    {
        let snap = repo.config_snapshot();
        if let Some(n) = snap.integer("diff.statNameWidth") {
            if n > 0 {
                stat_widths.name_width = n;
            }
        }
        if let Some(n) = snap.integer("diff.statGraphWidth") {
            if n > 0 {
                stat_widths.graph_width = n;
            }
        }
    }

    let mut max_count: Option<usize> = None;
    let mut skip: usize = 0;
    let mut pretty = Pretty::Medium;
    let mut terminator = false;
    // `--abbrev[=<n>]`, applied as a `core.abbrev` override so every abbreviation
    // in the run — `%h`, oneline ids, diff index lines — reads the same length.
    let mut abbrev_len: Option<usize> = None;
    // `rev->pretty_given`: the built-in formats show notes only when the caller
    // did not pick a format, so the flag has to be tracked, not inferred from
    // `pretty` (which starts at the same `medium` a `--pretty=medium` selects).
    let mut pretty_given = false;
    let mut notes_opt = super::notes::DisplayOpt::default();
    let mut abbrev_commit = cfg_abbrev_commit;
    let mut name_only = false;
    let mut name_status = false;
    let mut stat = false;
    let mut numstat = false;
    let mut shortstat = false;
    let mut patch = false;
    // `-q`/`--quiet`: git pre-sets DIFF_FORMAT_NO_OUTPUT before the other diff-format
    // flags parse, so it is position-independent. On `git log` (which shows no diff by
    // default) its only observable effect is the name-only/name-status conflict below.
    let mut quiet = false;
    // `--source`: annotate each commit with the ref/argument it was first reached
    // from (`\t<source>` after the hash), for the built-in header formats.
    let mut source_mode = false;
    let mut graph = false;
    // git's built-in default is `auto` (short refs when interactive, none when
    // piped); `log.decorate` overrides it, and the `--decorate` flags override
    // that in turn.
    let builtin_decorate = if std::io::stdout().is_terminal() {
        DecorateStyle::Short
    } else {
        DecorateStyle::Off
    };
    let mut decorate = cfg_decorate.unwrap_or(builtin_decorate);
    // `--decorate-refs=<pattern>` / `--decorate-refs-exclude=<pattern>` (both
    // repeatable) and `--clear-decorations`, which empties them again and drops
    // git's default "known namespaces" include list.
    let mut decorate_refs: Vec<String> = Vec::new();
    let mut decorate_refs_exclude: Vec<String> = Vec::new();
    let mut default_decoration_filter = true;
    // `--use-mailmap`/`--mailmap`: route the author/committer identity of the
    // built-in header formats through `.mailmap`. Seeded from `log.mailmap`.
    let mut use_mailmap = cfg_mailmap;
    let mut all = false;
    let mut reverse = false;
    let mut only_merges = false;
    let mut no_merges = false;
    let mut first_parent = false;
    let mut show_parents = false;
    let mut show_children = false;
    let mut boundary = false;
    // `--simplify-by-decoration`, plus somewhere to keep the decoration map when
    // the format itself did not ask for one.
    let mut simplify_by_decoration = false;
    let decorations_for_simplify: Option<Decorations>;
    let mut min_parents: Option<usize> = None;
    let mut max_parents: Option<usize> = None;
    let mut date_mode = cfg_date_mode;
    let mut show_root = cfg_show_root;
    let mut color = ColorWhen::Auto;
    let mut order = Order::Default;
    let mut revs: Vec<String> = Vec::new();
    let mut pathspecs: Vec<String> = Vec::new();
    // History filtering (`--grep`/`--author`/`--committer` + dialect flags),
    // matched through the shared `revfilter` so log and shortlog agree.
    let mut grep_pats: Vec<String> = Vec::new();
    let mut author_pats: Vec<String> = Vec::new();
    let mut committer_pats: Vec<String> = Vec::new();
    let mut grep_dialect = crate::revfilter::Dialect::Basic;
    let mut grep_ignore_case = false;
    let mut grep_all_match = false;
    let mut grep_invert = false;
    // `--since`/`--after` and `--until`/`--before` commit-date range (committer
    // time), parsed with git's approxidate.
    let mut since: Option<i64> = None;
    let mut until: Option<i64> = None;
    // Pickaxe: `-S<string>` (net occurrence count changed) / `-G<regex>` (a
    // changed line matches). Both diff each commit against its first parent.
    let mut pickaxe_s: Option<String> = None;
    let mut pickaxe_g: Option<String> = None;
    // `--diff-merges=<mode>`: what a *merge* commit's patch shows. git's default is
    // `off`, which is why `git log -p` prints no diff for a merge at all.
    let mut diff_merges = DiffMerges::Off;
    // `--follow`: keep following the one pathspec across renames.
    let mut follow = false;
    // `-L<range>:<file>`, repeatable: line-level traversal (see `line_log`).
    let mut line_ranges: Vec<String> = Vec::new();
    // `-s`/`--no-patch` resets the diff output format to git's NO_OUTPUT. That is a
    // non-empty format, so `-L` does not fall back to its `DIFF_FORMAT_PATCH`
    // default after one — even though every individual format flag is off again.
    let mut saw_no_patch = false;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            // Everything after `--` is a pathspec, even tokens that look like
            // flags — git stops option parsing at the separator.
            pathspecs.extend(args[i + 1..].iter().cloned());
            break;
        } else if a == "-n" || a == "--max-count" {
            i += 1;
            let v = args
                .get(i)
                .ok_or_else(|| anyhow!("option `{a}` requires a value"))?;
            match parse_max_count(v) {
                Ok(mc) => max_count = mc,
                Err(()) => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--max-count=") {
            match parse_max_count(v) {
                Ok(mc) => max_count = mc,
                Err(()) => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if a == "--decorate" {
            decorate = DecorateStyle::Short;
        } else if let Some(m) = a.strip_prefix("--decorate=") {
            match parse_decoration_style(m) {
                Some(s) => decorate = s,
                None => {
                    eprintln!("fatal: invalid --decorate option: {m}");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if a == "--no-decorate" {
            decorate = DecorateStyle::Off;
        } else if a == "--decorate-refs" || a == "--decorate-refs-exclude" {
            // git's `OPT_STRING_LIST` also takes its value as the next argv token,
            // and its parse-options layer rejects a missing one with exit 129.
            i += 1;
            let Some(v) = args.get(i) else {
                eprintln!("error: option `{}' requires a value", &a[2..]);
                return Ok(ExitCode::from(129));
            };
            if a == "--decorate-refs" {
                decorate_refs.push(v.clone());
            } else {
                decorate_refs_exclude.push(v.clone());
            }
        } else if let Some(v) = a.strip_prefix("--decorate-refs=") {
            decorate_refs.push(v.to_string());
        } else if let Some(v) = a.strip_prefix("--decorate-refs-exclude=") {
            decorate_refs_exclude.push(v.to_string());
        } else if a == "--clear-decorations" {
            // git's `clear_decorations_callback`: forget every pattern given so
            // far and stop applying the default namespace filter, so refs outside
            // the known namespaces become decoratable.
            decorate_refs.clear();
            decorate_refs_exclude.clear();
            default_decoration_filter = false;
        } else if a == "--use-mailmap" || a == "--mailmap" {
            use_mailmap = true;
        } else if a == "--no-use-mailmap" || a == "--no-mailmap" {
            use_mailmap = false;
        } else if a == "--oneline" {
            pretty = Pretty::Oneline;
            terminator = true;
            abbrev_commit = true;
            pretty_given = true;
        // `--notes[=<ref>]` and its `--show-notes` spelling, plus `--no-notes`:
        // git`s `notes_callback`. A later flag overrides an earlier one, and an
        // explicit ref suppresses both the default tree and `notes.displayRef`.
        } else if a == "--notes" || a == "--show-notes" {
            notes_opt.enable_default();
            notes_opt.given = true;
        } else if let Some(v) = a
            .strip_prefix("--notes=")
            .or_else(|| a.strip_prefix("--show-notes="))
        {
            notes_opt.enable_ref(v);
            notes_opt.given = true;
        } else if a == "--no-notes" || a == "--no-show-notes" {
            notes_opt.disable();
            notes_opt.given = true;
        } else if let Some(v) = a.strip_prefix("--pretty=") {
            match get_commit_format(v)? {
                Some((p, t)) => {
                    pretty = p;
                    terminator = t;
                    pretty_given = true;
                }
                None => {
                    eprintln!("fatal: invalid --pretty format: {v}");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--format=") {
            // `--format=<s>` is git`s alias for `--pretty=<s>` (same parser, not a
            // blind `tformat:` wrapper — `--format=abc` is rejected just like
            // `--pretty=abc`).
            match get_commit_format(v)? {
                Some((p, t)) => {
                    pretty = p;
                    terminator = t;
                    pretty_given = true;
                }
                None => {
                    eprintln!("fatal: invalid --pretty format: {v}");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if a == "--pretty" {
            // Bare `--pretty` is git`s `--pretty=medium`.
            pretty = Pretty::Medium;
            terminator = false;
            pretty_given = true;
        } else if a == "--format" {
            // Bare `--format` (no `=value`) is a git usage error, exit 128.
            eprintln!("fatal: unrecognized argument: --format");
            return Ok(ExitCode::from(128));
        } else if a == "--skip" {
            i += 1;
            let v = args
                .get(i)
                .ok_or_else(|| anyhow!("option `{a}` requires a value"))?;
            match parse_nonneg(v) {
                Some(n) => skip = n,
                None => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--skip=") {
            match parse_nonneg(v) {
                Some(n) => skip = n,
                None => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--date=") {
            match parse_date_mode(v) {
                Some(m) => date_mode = m,
                None => {
                    eprintln!("fatal: unknown date format {v}");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--min-parents=") {
            match parse_nonneg(v) {
                Some(n) => min_parents = Some(n),
                None => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("--max-parents=") {
            match parse_nonneg(v) {
                Some(n) => max_parents = Some(n),
                None => {
                    eprintln!("fatal: '{v}': not an integer");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if a == "--no-min-parents" {
            min_parents = Some(0);
        } else if a == "--no-max-parents" {
            max_parents = None;
        } else if a == "--first-parent" {
            first_parent = true;
        } else if a == "--parents" {
            show_parents = true;
        } else if a == "--simplify-by-decoration" {
            simplify_by_decoration = true;
        } else if a == "--boundary" {
            boundary = true;
        } else if a == "--no-boundary" {
            boundary = false;
        } else if a == "--children" {
            show_children = true;
        } else if a == "--no-children" {
            show_children = false;
        } else if a == "--abbrev-commit" {
            abbrev_commit = true;
        } else if a == "--no-abbrev-commit" {
            abbrev_commit = false;
        // `--abbrev[=<n>]` / `--no-abbrev`: the length every abbreviated id in the
        // run is cut to. git clamps below `MINIMUM_ABBREV` (4) and at the hash
        // width, and `--no-abbrev` is the full width. It reaches `%h`, the
        // oneline id and the diff `index` lines — but not the `commit <id>`
        // header, which only `--abbrev-commit` shortens.
        } else if a == "--abbrev" {
            abbrev_len = Some(DEFAULT_ABBREV);
        } else if a == "--no-abbrev" {
            abbrev_len = Some(repo.object_hash().len_in_hex());
        } else if let Some(v) = a.strip_prefix("--abbrev=") {
            let full = repo.object_hash().len_in_hex();
            match v.parse::<usize>() {
                Ok(n) => abbrev_len = Some(n.clamp(MINIMUM_ABBREV, full)),
                Err(_) => {
                    eprintln!("fatal: option `abbrev' expects a numerical value");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if a == "-p" || a == "--patch" || a == "-u" {
            // `-u` is git's documented synonym for `-p`.
            patch = true;
        } else if a == "-q" || a == "--quiet" {
            // Position-independent NO_OUTPUT (git applies it before `setup_revisions`
            // parses `-p`/`--stat`), so a later or earlier format flag always wins.
            quiet = true;
        } else if a == "--no-quiet" {
            quiet = false;
        } else if a == "--source" {
            source_mode = true;
        } else if a == "--no-source" {
            source_mode = false;
        } else if a == "-L" {
            // git's `OPT_CALLBACK('L', ...)` takes its value as the next argv token
            // and its parse-options layer rejects a missing one with exit 129.
            i += 1;
            let Some(v) = args.get(i) else {
                eprintln!("error: switch `L' requires a value");
                return Ok(ExitCode::from(129));
            };
            line_ranges.push(v.clone());
        } else if let Some(v) = a.strip_prefix("-L") {
            line_ranges.push(v.to_string());
        } else if a == "-s" || a == "--no-patch" {
            // Suppress diff output — git treats `-s` as order-sensitive, so a
            // later `--stat`/`-p` re-enables whichever format follows it.
            saw_no_patch = true;
            stat = false;
            numstat = false;
            shortstat = false;
            name_only = false;
            name_status = false;
            patch = false;
        } else if a == "--name-only" {
            name_only = true;
        } else if a == "--name-status" {
            name_status = true;
        } else if a == "--stat" {
            stat = true;
        } else if let Some(v) = a.strip_prefix("--stat=") {
            // `--stat[=<width>[,<name-width>[,<count>]]]`: sets the total width (and
            // optionally the name column / line cap) and, like every `--stat*` flag,
            // requests the diffstat.
            stat = true;
            parse_stat_geometry(&mut stat_widths, v);
        } else if let Some(v) = a.strip_prefix("--stat-width=") {
            stat = true;
            stat_widths.width = parse_stat_i64(v);
        } else if let Some(v) = a.strip_prefix("--stat-name-width=") {
            stat = true;
            stat_widths.name_width = parse_stat_i64(v);
        } else if let Some(v) = a.strip_prefix("--stat-graph-width=") {
            stat = true;
            stat_widths.graph_width = parse_stat_i64(v);
        } else if let Some(v) = a.strip_prefix("--stat-count=") {
            stat = true;
            stat_widths.count = parse_stat_i64(v);
        } else if a == "--numstat" {
            numstat = true;
        } else if a == "--shortstat" {
            shortstat = true;
        } else if a == "--root" {
            // Force the root commit's diff on (a diff against the empty tree),
            // overriding `log.showRoot=false`. git has no `--no-root`.
            show_root = true;
        } else if a == "--graph" {
            graph = true;
        } else if a == "--all" {
            all = true;
        } else if a == "--reverse" {
            reverse = true;
        } else if a == "--merges" {
            only_merges = true;
        } else if a == "--no-merges" {
            no_merges = true;
        } else if a == "--color" {
            // Bare `--color` is git's `--color=always`.
            color = ColorWhen::Always;
        } else if a == "--no-color" {
            color = ColorWhen::Never;
        } else if let Some(v) = a.strip_prefix("--color=") {
            match v {
                "always" => color = ColorWhen::Always,
                "never" => color = ColorWhen::Never,
                "auto" => color = ColorWhen::Auto,
                _ => {
                    eprintln!("fatal: invalid color value: {v}");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if a == "--date-order" {
            order = Order::Date;
        } else if a == "--topo-order" {
            order = Order::Topo;
        } else if let Some(v) = a.strip_prefix("--grep=") {
            grep_pats.push(v.to_string());
        } else if a == "--grep" {
            i += 1;
            grep_pats.push(args.get(i).cloned().unwrap_or_default());
        } else if let Some(v) = a.strip_prefix("--author=") {
            author_pats.push(v.to_string());
        } else if a == "--author" {
            i += 1;
            author_pats.push(args.get(i).cloned().unwrap_or_default());
        } else if let Some(v) = a.strip_prefix("--committer=") {
            committer_pats.push(v.to_string());
        } else if a == "--committer" {
            i += 1;
            committer_pats.push(args.get(i).cloned().unwrap_or_default());
        } else if a == "-i" || a == "--regexp-ignore-case" {
            grep_ignore_case = true;
        } else if a == "-E" || a == "--extended-regexp" {
            grep_dialect = crate::revfilter::Dialect::Extended;
        } else if a == "-F" || a == "--fixed-strings" {
            grep_dialect = crate::revfilter::Dialect::Fixed;
        } else if a == "-P" || a == "--perl-regexp" {
            grep_dialect = crate::revfilter::Dialect::Perl;
        } else if a == "--basic-regexp" {
            grep_dialect = crate::revfilter::Dialect::Basic;
        } else if a == "--all-match" {
            grep_all_match = true;
        } else if a == "--invert-grep" {
            grep_invert = true;
        } else if let Some(v) = a.strip_prefix("--since=").or_else(|| a.strip_prefix("--after=")) {
            since = Some(approxidate(v));
        } else if a == "--since" || a == "--after" {
            i += 1;
            since = Some(approxidate(&args.get(i).cloned().unwrap_or_default()));
        } else if let Some(v) = a
            .strip_prefix("--until=")
            .or_else(|| a.strip_prefix("--before="))
        {
            until = Some(approxidate(v));
        } else if a == "--until" || a == "--before" {
            i += 1;
            until = Some(approxidate(&args.get(i).cloned().unwrap_or_default()));
        } else if a == "-S" {
            i += 1;
            pickaxe_s = Some(args.get(i).cloned().unwrap_or_default());
        } else if let Some(v) = a.strip_prefix("-S") {
            pickaxe_s = Some(v.to_string());
        } else if a == "-G" {
            i += 1;
            pickaxe_g = Some(args.get(i).cloned().unwrap_or_default());
        } else if a == "--follow" {
            follow = true;
        } else if a == "--no-follow" {
            follow = false;
        } else if a == "-m" {
            // `diff_merges_set_dense_combined_if_unset()` and friends
            // (diff-merges.c): each spelling selects a mode, and the last one wins.
            diff_merges = DiffMerges::Separate;
        } else if a == "-c" {
            diff_merges = DiffMerges::Combined;
        } else if a == "--cc" {
            diff_merges = DiffMerges::DenseCombined;
        } else if a == "--no-diff-merges" {
            diff_merges = DiffMerges::Off;
        } else if let Some(v) = a.strip_prefix("--diff-merges=") {
            match DiffMerges::parse(v) {
                Some(m) => diff_merges = m,
                None => {
                    eprintln!("fatal: invalid value for '--diff-merges': '{v}'");
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some(v) = a.strip_prefix("-G") {
            pickaxe_g = Some(v.to_string());
        } else if let Some(body) = a.strip_prefix('-') {
            if let Some(num) = body.strip_prefix('n') {
                // `-nN` shorthand (e.g. `-n5`).
                match parse_max_count(num) {
                    Ok(mc) => max_count = mc,
                    Err(()) => {
                        eprintln!("fatal: '{num}': not an integer");
                        return Ok(ExitCode::from(128));
                    }
                }
            } else if !body.is_empty() && body.bytes().all(|c| c.is_ascii_digit()) {
                // `-N` shorthand (e.g. `-5`): show N commits, so N is positive.
                match parse_max_count(body) {
                    Ok(mc) => max_count = mc,
                    Err(()) => {
                        eprintln!("fatal: '{body}': not an integer");
                        return Ok(ExitCode::from(128));
                    }
                }
            } else {
                bail!("unsupported flag {a:?}");
            }
        } else {
            // A non-flag token before `--` is a revision; git accepts several and
            // walks the union of their histories.
            revs.push(a.clone());
        }
        i += 1;
    }

    // `-L` (`rev->line_level_traverse`). git rejects the combinations it cannot
    // render in `setup_revisions`, before the pathspec check in `cmd_log_init_finish`.
    let line_level = !line_ranges.is_empty();
    if line_level {
        // git's allowed set is PATCH / NO_OUTPUT / RAW / NAME / NAME_STATUS /
        // SUMMARY; the count formats are not in it.
        if stat || numstat || shortstat {
            eprintln!("fatal: -L does not yet support the requested diff format");
            return Ok(ExitCode::from(128));
        }
        if !pathspecs.is_empty() {
            eprintln!("fatal: -L<range>:<file> cannot be used with pathspec");
            return Ok(ExitCode::from(128));
        }
        // `if (!revs->diffopt.output_format) output_format = DIFF_FORMAT_PATCH;`
        if !patch && !name_only && !name_status && !saw_no_patch && !quiet {
            patch = true;
        }
    }

    // git checks this combination before touching the repository.
    if graph && reverse {
        eprintln!("fatal: options '--graph' and '--reverse' cannot be used together");
        return Ok(ExitCode::from(128));
    }

    // git's `diff_setup_done` rejects using more than one of `--name-only`,
    // `--name-status`, `--check`, and `-s` (NO_OUTPUT) together. `--quiet` pre-sets
    // NO_OUTPUT, but the stat/patch output formats clear it again, so `--quiet`
    // counts toward this conflict only when none of them are present (matching
    // `git log --name-only --stat --quiet`, which git accepts).
    let quiet_no_output = quiet && !patch && !stat && !numstat && !shortstat;
    if name_only as u8 + name_status as u8 + quiet_no_output as u8 > 1 {
        eprintln!(
            "fatal: options '--name-only', '--name-status', '--check', and '-s' cannot be used together"
        );
        return Ok(ExitCode::from(128));
    }

    // Collect the starting tips in git's order: the named revision (or HEAD),
    // then every ref sorted by full name, then HEAD again for `--all`.
    let mut tips: Vec<ObjectId> = Vec::new();
    // Parallel to `tips` and populated only under `--source`: the name each tip was
    // reached from (a rev argument, a full refname for `--all`, or `HEAD`). A commit
    // inherits the source of the tip that first reaches it during the walk.
    let mut tip_sources: Vec<String> = Vec::new();
    // Parallel to `tips`: the argument or refname each was named by, which is what
    // `check_single_commit`'s "More than one commit to dig from" reports under `-L`.
    let mut tip_names: Vec<String> = Vec::new();
    // Split each revision arg into positive tips and negative (excluded) tips to
    // support git's range forms: `A..B` (= `^A B`), `A...B` (symmetric difference —
    // exclude the merge-base), and a leading `^A`. An empty endpoint means `HEAD`
    // (`A..`, `..B`). Anything without `..`/`^` is a single positive tip, as before.
    let mut pos_specs: Vec<String> = Vec::new();
    let mut neg_ids: Vec<ObjectId> = Vec::new();
    for spec in &revs {
        if let Some(rest) = spec.strip_prefix('^') {
            match resolve_rev(&repo, rest) {
                Ok(id) => neg_ids.push(id),
                Err(_) => {
                    let hex_len = repo.object_hash().len_in_hex();
                    eprint!("{}", bad_revision_message(rest, hex_len));
                    return Ok(ExitCode::from(128));
                }
            }
        } else if let Some((a, b)) = spec.split_once("...") {
            let a = if a.is_empty() { "HEAD" } else { a };
            let b = if b.is_empty() { "HEAD" } else { b };
            pos_specs.push(a.to_string());
            pos_specs.push(b.to_string());
            // `A...B` hides what both endpoints can reach: their merge-base.
            if let (Ok(ia), Ok(ib)) = (resolve_rev(&repo, a), resolve_rev(&repo, b)) {
                if let Ok(base) = repo.merge_base(ia, ib) {
                    neg_ids.push(base.detach());
                }
            }
        } else if let Some((a, b)) = spec.split_once("..") {
            let a = if a.is_empty() { "HEAD" } else { a };
            let b = if b.is_empty() { "HEAD" } else { b };
            match resolve_rev(&repo, a) {
                Ok(id) => neg_ids.push(id),
                Err(_) => {
                    let hex_len = repo.object_hash().len_in_hex();
                    eprint!("{}", bad_revision_message(a, hex_len));
                    return Ok(ExitCode::from(128));
                }
            }
            pos_specs.push(b.to_string());
        } else {
            pos_specs.push(spec.clone());
        }
    }
    // git resolves each positional token as a revision; the first that is *not* a
    // revision but names an existing path switches to pathspec mode — that token and
    // every one after it become pathspecs, exactly as if a `--` had preceded them
    // (so `git log .` == `git log -- .`). A token that is neither a revision nor a
    // path is the "ambiguous argument" fatal.
    let mut in_paths = false;
    for spec in &pos_specs {
        if in_paths {
            pathspecs.push(spec.clone());
            continue;
        }
        match repo.rev_parse_single(spec.as_str()) {
            Ok(id) => {
                tips.push(peel_to_commit(&repo, id.detach()));
                tip_names.push(spec.clone());
                if source_mode {
                    tip_sources.push(spec.clone());
                }
            }
            Err(_) if spec_is_path(&repo, spec) => {
                in_paths = true;
                pathspecs.push(spec.clone());
            }
            Err(_) => {
                let hex_len = repo.object_hash().len_in_hex();
                eprint!("{}", bad_revision_message(spec, hex_len));
                return Ok(ExitCode::from(128));
            }
        }
    }
    // Whether the args named any *positive* revision tip. When they didn't — no revs,
    // only `^exclude`s, or every token turned out to be a pathspec (`git log .`) —
    // git defaults the positive side to `HEAD`, just as for a bare `git log`.
    let positive_from_args = !tips.is_empty();
    if all {
        // Materialise the names first: the iterator holds the packed-refs buffer,
        // which would block the per-ref object lookups below.
        let mut names: Vec<Vec<u8>> = Vec::new();
        for r in repo.references()?.all()? {
            let r = r.map_err(|e| anyhow!("{e}"))?;
            names.push(r.name().as_bstr().to_vec());
        }
        // git walks `refs/` in sorted full-name order, which decides the tie-break
        // between tips that share a commit date.
        names.sort();
        for name in names {
            let Ok(full) = name.to_str() else { continue };
            let Ok(reference) = repo.find_reference(full) else {
                continue;
            };
            let Ok(id) = reference.into_fully_peeled_id() else {
                continue;
            };
            let oid = id.detach();
            // A tag pointing at a tree or blob is not a history tip.
            if let Ok(obj) = repo.find_object(oid) {
                if obj.kind == gix::objs::Kind::Commit {
                    tips.push(oid);
                    tip_names.push(full.to_string());
                    if source_mode {
                        tip_sources.push(full.to_string());
                    }
                }
            }
        }
    }
    if !positive_from_args || all {
        let head = repo.head()?;
        if head.is_unborn() && !all {
            let branch = head
                .referent_name()
                .map(|n| n.shorten().to_str_lossy().into_owned())
                .unwrap_or_else(|| "master".to_owned());
            eprintln!("fatal: your current branch '{branch}' does not have any commits yet");
            return Ok(ExitCode::from(128));
        }
        if let Some(id) = repo.head()?.try_peel_to_id()? {
            tips.push(id.detach());
            tip_names.push("HEAD".to_string());
            if source_mode {
                tip_sources.push("HEAD".to_string());
            }
        }
    }

    // Walk in git's default commit-date order, then re-sort if a topological
    // order was asked for. `--graph` implies `--topo-order` unless `--date-order`
    // was given explicitly.
    // Commits reachable from the negative tips are hidden from the walk (the `..`
    // range exclusion). Empty when no `A..B`/`^A` was given.
    let hidden = if neg_ids.is_empty() {
        HashSet::new()
    } else {
        ancestor_closure(&repo, &neg_ids)?
    };
    // The walk may stop early only when every commit it yields is guaranteed to
    // be shown: no pathspec, parent-count, date, grep or pickaxe filter can drop
    // one, no topological re-sort needs the whole set, and `--reverse` does not
    // need the tail. Anything else walks the full history as before.
    let unfiltered = pathspecs.is_empty()
        && !line_level
        && !only_merges
        && !no_merges
        && min_parents.is_none()
        && max_parents.is_none()
        && since.is_none()
        && until.is_none()
        && author_pats.is_empty()
        && committer_pats.is_empty()
        && grep_pats.is_empty()
        && pickaxe_s.is_none()
        && pickaxe_g.is_none()
        && !reverse
        && !graph
        && order == Order::Default;
    let budget = (unfiltered && max_count.is_some())
        .then(|| skip.saturating_add(max_count.unwrap_or(0)));
    let mut nodes = walk(&repo, &tips, &tip_sources, first_parent, &hidden, budget)?;
    // `-L` sets `revs->topo_order = 1` without touching `sort_order`, so it walks
    // topologically unless `--date-order` asked for the date-ordered variant.
    let effective_order = match (order, graph || line_level) {
        (Order::Default, true) => Order::Topo,
        (o, _) => o,
    };
    if effective_order != Order::Default {
        nodes = topo_sort(nodes, effective_order == Order::Date);
    }

    // `-L`: carry the tracked ranges backward through the history, keeping only the
    // commits that took blame for one. The file pairs a kept commit is responsible
    // for are held for the output pass below.
    let mut line_log_pairs: HashMap<ObjectId, Vec<(line_log::Pair, Vec<line_log::Range>)>> =
        HashMap::new();
    if line_level {
        // A positional token that turned out to name a path only becomes a pathspec
        // during the loop above, which is why this repeats the earlier check.
        if !pathspecs.is_empty() {
            eprintln!("fatal: -L<range>:<file> cannot be used with pathspec");
            return Ok(ExitCode::from(128));
        }
        // `check_single_commit`: the ranges are resolved against exactly one commit,
        // so several positive tips leave the starting blob undefined.
        if tips.len() > 1 {
            eprintln!(
                "fatal: More than one commit to dig from: {} and {}?",
                tip_names.get(1).map(String::as_str).unwrap_or_default(),
                tip_names.first().map(String::as_str).unwrap_or_default()
            );
            return Ok(ExitCode::from(128));
        }
        let Some(start) = tips.first().copied() else {
            eprintln!("fatal: No commit specified?");
            return Ok(ExitCode::from(128));
        };
        let tracked = match line_log::parse_lines(&repo, start, &line_ranges) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("fatal: {}", e.0);
                return Ok(ExitCode::from(128));
            }
        };
        let mut tracker = line_log::Tracker::new(&repo, start, tracked, first_parent);
        let mut kept = Vec::with_capacity(nodes.len());
        // Every walked commit's post-line-log parent list plus whether it survived,
        // which is what `line_log_rewrite_one` reads (a dropped commit is git's
        // TREESAME).
        let mut seen: HashMap<ObjectId, (Vec<ObjectId>, bool)> = HashMap::new();
        for mut node in nodes.into_iter() {
            let (range, parents) = tracker.process(node.id, &node.parents)?;
            node.parents = parents;
            seen.insert(node.id, (node.parents.clone(), range.is_some()));
            if let Some(range) = range {
                line_log_pairs.insert(node.id, line_log::queue_pairs(&range));
                kept.push(node);
            }
        }
        // `line_log_filter` finishes with `rewrite_parents()`, which git runs only
        // when the caller wants ancestry — `--graph` and `--parents` are what set
        // `rewrite_parents` here. Every other format never prints a parent.
        if graph || show_parents {
            for node in &mut kept {
                let mut rewritten: Vec<ObjectId> = Vec::with_capacity(node.parents.len());
                for p in &node.parents {
                    if let Some(id) = line_log_rewrite_one(*p, &seen, &hidden) {
                        if !rewritten.contains(&id) {
                            rewritten.push(id);
                        }
                    }
                }
                node.parents = rewritten;
            }
        }
        nodes = kept;
    }

    // Path-limited traversal, a port of `try_to_simplify_commit()` followed by
    // `rewrite_parents()` (revision.c).
    //
    // The test is TREESAME *per parent*, not against the first one: a commit that
    // matches any parent over the pathspec is "simplified away" — it is not
    // shown, and the history it stands for is the one behind that parent alone.
    // For a merge that is what removes both the merge itself and the entire side
    // whose changes it did not take; comparing only against the first parent
    // leaves the merge in the log and lists the other side's commits as well.
    if follow {
        // `cmd_log_init_finish()`: `--follow` rewrites the pathspec as the walk
        // goes back, so it can only track one path.
        if pathspecs.len() != 1 {
            eprintln!("fatal: --follow requires exactly one pathspec");
            return Ok(ExitCode::from(128));
        }
        // `try_to_follow_renames()` (tree-diff.c): walk newest first along the
        // first parent, and when the followed path turns out to have arrived by a
        // rename, switch to the name it came from. A commit is shown exactly when
        // the followed path changed in it.
        let mut current: gix::bstr::BString = pathspecs[0].clone().into();
        let mut shown: Vec<Node> = Vec::new();
        for node in std::mem::take(&mut nodes) {
            let commit = repo.find_object(node.id)?.try_into_commit()?;
            let parent = node.parents.first().copied();
            let spec = vec![current.to_string()];
            let changed = changes_match(&repo, &commit, parent, &spec)?;
            if changed {
                let mut node = node;
                node.follow_path = Some(current.clone());
                shown.push(node);
            }
            // The switch happens after the commit is judged: the rename *is* the
            // change that makes the commit interesting.
            if let Some(parent) = parent {
                if let Some(src) = follow_source(&repo, &commit, parent, &current)? {
                    current = src;
                }
            }
        }
        nodes = shown;
    } else if !pathspecs.is_empty() {
        // id → (parents the simplified history follows, whether it is shown).
        let mut simplified: HashMap<ObjectId, (Vec<ObjectId>, bool)> =
            HashMap::with_capacity(nodes.len());
        for node in &nodes {
            let commit = repo.find_object(node.id)?.try_into_commit()?;
            // `--first-parent` limits the comparison the same way it limits the
            // walk: git never looks at the parents it is not following.
            let parents: &[ObjectId] = if first_parent {
                &node.parents[..node.parents.len().min(1)]
            } else {
                &node.parents
            };
            if parents.is_empty() {
                // A root commit is compared against the empty tree, so it shows
                // exactly when it introduced a matching path.
                let shown = changes_match(&repo, &commit, None, &pathspecs)?;
                simplified.insert(node.id, (Vec::new(), shown));
                continue;
            }
            let mut treesame: Option<ObjectId> = None;
            for p in parents {
                if !changes_match(&repo, &commit, Some(*p), &pathspecs)? {
                    treesame = Some(*p);
                    break;
                }
            }
            match treesame {
                Some(p) => simplified.insert(node.id, (vec![p], false)),
                None => simplified.insert(node.id, (parents.to_vec(), true)),
            };
        }
        // Whatever the simplified parent lists no longer reach was never walked
        // by git in the first place, so it cannot appear in the output.
        let mut reachable: HashSet<ObjectId> = HashSet::with_capacity(nodes.len());
        let mut stack: Vec<ObjectId> = tips.clone();
        while let Some(id) = stack.pop() {
            if !reachable.insert(id) {
                continue;
            }
            if let Some((parents, _)) = simplified.get(&id) {
                stack.extend(parents.iter().copied());
            }
        }
        nodes.retain(|n| {
            reachable.contains(&n.id) && simplified.get(&n.id).is_some_and(|(_, shown)| *shown)
        });
        // `rewrite_parents()`: the ancestry the output shows is the simplified
        // one, and a parent reachable from another parent drops out of it. Only
        // the ancestry-printing formats take it: the per-commit diff stays
        // against the real first parent, which is what `log --name-status --
        // <path>` reports.
        if graph || show_parents {
            for node in &mut nodes {
                let mut rewritten: Vec<ObjectId> = Vec::with_capacity(node.parents.len());
                for p in &node.parents {
                    if let Some(id) = simplify_rewrite_one(*p, &simplified) {
                        if !rewritten.contains(&id) {
                            rewritten.push(id);
                        }
                    }
                }
                prune_redundant_parents(&repo, &mut rewritten);
                node.parents = rewritten;
            }
        }
    }

    // `--merges`/`--no-merges` are git's aliases for `--min-parents=2` /
    // `--max-parents=1`; parent-count limiting happens before commit limiting.
    if only_merges {
        nodes.retain(|n| n.parents.len() >= 2);
    }
    if no_merges {
        nodes.retain(|n| n.parents.len() < 2);
    }
    if let Some(min) = min_parents {
        nodes.retain(|n| n.parents.len() >= min);
    }
    if let Some(max) = max_parents {
        nodes.retain(|n| n.parents.len() <= max);
    }

    // `--grep`/`--author`/`--committer` header/message filtering, applied during
    // selection — before `--skip`/`--max-count`, exactly as git does.
    let commit_filter = crate::revfilter::CommitFilter {
        author_res: crate::revfilter::compile_patterns(&author_pats, grep_dialect, grep_ignore_case)?,
        committer_res: crate::revfilter::compile_patterns(
            &committer_pats,
            grep_dialect,
            grep_ignore_case,
        )?,
        grep_res: crate::revfilter::compile_patterns(&grep_pats, grep_dialect, grep_ignore_case)?,
        all_match: grep_all_match,
        invert_grep: grep_invert,
    };
    // Pickaxe `-G<regex>` compiles once, in the same dialect as --grep.
    let pickaxe_g_re = match &pickaxe_g {
        Some(p) => Some(crate::revfilter::build_regex(p, grep_dialect, grep_ignore_case)?),
        None => None,
    };
    let has_pickaxe = pickaxe_s.is_some() || pickaxe_g_re.is_some();

    if !commit_filter.is_empty() || since.is_some() || until.is_some() || has_pickaxe {
        let mut kept = Vec::with_capacity(nodes.len());
        for node in nodes.into_iter() {
            let commit = repo.find_commit(node.id)?;
            // `--since`/`--until` gate on committer time (git's default), then
            // the header/message predicates.
            let seconds = commit.time()?.seconds;
            if since.is_some_and(|s| seconds < s) || until.is_some_and(|u| seconds > u) {
                continue;
            }
            if !commit_filter.matches(&commit)? {
                continue;
            }
            kept.push(node);
        }
        // Pickaxe: test each surviving commit's changes against `-S`/`-G`. Both
        // scans run across the thread pool — the commits are independent, and git
        // walks the same candidates one at a time on one core.
        if has_pickaxe {
            // A merge produces no diff without `-m`/`-c`/`--cc`, and the pickaxe
            // tests a diff — so git never reports a merge for `-S`/`-G` no matter
            // what its parents contain. Dropping them here also keeps the scan
            // from reading blobs for the largest commits in the history.
            kept.retain(|n| n.parents.len() < 2);
            kept = match (&pickaxe_s, &pickaxe_g_re) {
                // `-S` alone never needs patch text. git's `has_changes` counts
                // the needle in each side's whole blob and keeps the file when the
                // two counts differ, so the scan reads blobs and never diffs them.
                (Some(needle), None) => pickaxe_by_count(&repo, kept, needle.as_bytes())?,
                _ => {
                    let jobs: Vec<(ObjectId, Option<ObjectId>)> =
                        kept.iter().map(|n| (n.id, n.parents.first().copied())).collect();
                    let patches = super::diff::commit_patches(&repo, &jobs, 0, &pathspecs)?;
                    kept.into_iter()
                        .zip(patches)
                        .filter(|(_, patch)| {
                            pickaxe_hit(patch, pickaxe_s.as_deref(), pickaxe_g_re.as_ref())
                        })
                        .map(|(node, _)| node)
                        .collect()
                }
            };
        }
        nodes = kept;
    }

    // `--skip` drops the first N of the selected commits, then `--max-count` caps
    // what remains — git's order in `get_revision`.
    if skip > 0 {
        let drop = skip.min(nodes.len());
        nodes.drain(0..drop);
    }
    if let Some(limit) = max_count {
        nodes.truncate(limit);
    }
    if reverse {
        nodes.reverse();
    }

    if graph && nodes.iter().any(|n| n.parents.len() > 2) {
        bail!("--graph is not ported for octopus merges");
    }

    // `--name-only`/`--name-status` are git's reported format; they suppress both
    // the count formats and the `-p` patch. The patch is emitted after the count
    // formats otherwise.
    // `diff_merges_setup_revs()`: `-c`/`--cc` set `merges_imply_patch`, and the
    // patch becomes the format only when nothing else claimed it
    // (`if (!revs->diffopt.output_format)`), so `-c --stat` stays a stat. `-m` sets
    // no such flag, which is why `git log -m` on its own still prints no diff.
    let patch = patch
        || (matches!(diff_merges, DiffMerges::Combined | DiffMerges::DenseCombined)
            && !(stat || numstat || shortstat || name_only || name_status));
    let emit_patch = patch && !name_only && !name_status;
    let want_names = name_only || name_status || stat || numstat || shortstat;
    // Whether `%C`/`%d` emit ANSI: git's auto rule is "stdout is a terminal, or we
    // are paging to one" — `pager::maybe_setup` records the latter via the env flag.
    let want_color = match color {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        // git routes `git log`'s coloring through the diff machinery, so the
        // config switch is `color.diff` falling back to `color.ui`; `auto` then
        // asks whether stdout is a terminal or a `color.pager` pager.
        ColorWhen::Auto => super::color::want_color_stdout(&repo, "diff"),
    };
    // `%d`/`%D` need a commit→refs map; build it only when the format asks for one
    // so plain formats pay nothing for the ref scan.
    let decorations = if pretty_uses_decoration(&pretty) || decorate != DecorateStyle::Off {
        let filter = DecorationFilter::build(
            &repo,
            &decorate_refs,
            &decorate_refs_exclude,
            default_decoration_filter,
        );
        Some(build_decorations(&repo, &filter)?)
    } else {
        None
    };
    // git's `color.decorate.<slot>` table, plus the `color.diff.commit` color it
    // paints the decoration punctuation and the commit object name with. Resolved
    // once; the disabled table when this run is not coloring at all.
    let deco_colors = if want_color {
        super::color::DecorateColors::resolve(&repo)
    } else {
        super::color::DecorateColors::disabled()
    };
    // `--use-mailmap` / `log.mailmap`: loaded once (worktree `.mailmap`, then
    // `mailmap.blob`, then `mailmap.file`) and shared by every rendered record.
    let mailmap = use_mailmap.then(|| Mailmap::load(&repo));
    // `revision.c`: the two ancestry decorations share one slot in the header and
    // git refuses to print both rather than pick an order.
    if show_parents && show_children {
        eprintln!("fatal: options '--parents' and '--children' cannot be used together");
        return Ok(ExitCode::from(128));
    }

    // `--simplify-by-decoration`: the same simplification the pathspec path runs,
    // with a different question asked of each commit. `simplify_commit()` keeps a
    // commit that carries a decoration, and — since simplification may not drop
    // the shape of the history — a root or a merge; everything else is walked
    // past. The parent lists are rewritten so `--graph`/`--parents` draw the
    // simplified history rather than the real one.
    if simplify_by_decoration {
        let decos = match &decorations {
            Some(d) => d,
            None => {
                let filter = DecorationFilter::build(
                    &repo,
                    &decorate_refs,
                    &decorate_refs_exclude,
                    default_decoration_filter,
                );
                decorations_for_simplify = Some(build_decorations(&repo, &filter)?);
                decorations_for_simplify.as_ref().expect("just built")
            }
        };
        let mut simplified: HashMap<ObjectId, (Vec<ObjectId>, bool)> =
            HashMap::with_capacity(nodes.len());
        for node in &nodes {
            let shown =
                decos.decorates(&node.id) || node.parents.is_empty() || node.parents.len() > 1;
            let parents = if shown {
                node.parents.clone()
            } else {
                node.parents[..node.parents.len().min(1)].to_vec()
            };
            simplified.insert(node.id, (parents, shown));
        }
        let mut reachable: HashSet<ObjectId> = HashSet::with_capacity(nodes.len());
        let mut stack: Vec<ObjectId> = tips.clone();
        while let Some(id) = stack.pop() {
            if !reachable.insert(id) {
                continue;
            }
            if let Some((parents, _)) = simplified.get(&id) {
                stack.extend(parents.iter().copied());
            }
        }
        nodes.retain(|n| {
            reachable.contains(&n.id) && simplified.get(&n.id).is_some_and(|(_, shown)| *shown)
        });
        // `rewrite_parents()` runs whenever a simplification did: the ancestry the
        // output shows — the `Merge:` header, `--parents`, the graph — is the
        // simplified one, not the commit's real parent list.
        for node in &mut nodes {
            let mut rewritten: Vec<ObjectId> = Vec::with_capacity(node.parents.len());
            for p in &node.parents {
                if let Some(id) = simplify_rewrite_one(*p, &simplified) {
                    if !rewritten.contains(&id) {
                        rewritten.push(id);
                    }
                }
            }
            prune_redundant_parents(&repo, &mut rewritten);
            node.parents = rewritten;
        }
    }

    // `--boundary`: the excluded commits the shown history hangs off — every
    // parent that the exclusion hid — appended after the walk with a `-` mark.
    // git emits them from `revs->boundary_commits` once the main walk is done, so
    // they come last regardless of their dates and skip the filters above.
    if boundary && !hidden.is_empty() {
        let shown: HashSet<ObjectId> = nodes.iter().map(|n| n.id).collect();
        let mut seen: HashSet<ObjectId> = HashSet::new();
        let mut edge: Vec<Node> = Vec::new();
        let reader = NodeReader::new(&repo);
        for node in &nodes {
            for parent in &node.parents {
                if shown.contains(parent) || !hidden.contains(parent) || !seen.insert(*parent) {
                    continue;
                }
                let mut n = reader.read(&repo, *parent)?;
                n.boundary = true;
                edge.push(n);
            }
        }
        edge.sort_by_key(|n| std::cmp::Reverse(n.time));
        nodes.extend(edge);
    }

    // `--children`: git records a child on every parent as it walks, so the list
    // names only commits this run reached.
    let children: Option<HashMap<ObjectId, Vec<ObjectId>>> = show_children.then(|| {
        let mut map: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
        for node in &nodes {
            for parent in &node.parents {
                // `push_children()` splices each child onto the *front* of the
                // list, so the ids come out in reverse walk order.
                map.entry(*parent).or_default().insert(0, node.id);
            }
        }
        map
    });

    // `--abbrev=<n>` is `revs->abbrev`, which every abbreviation in the run reads.
    // Pushing it into the repository's config as `core.abbrev` puts it in front of
    // the same lookup gitoxide already makes, so `%h`, the oneline id and the diff
    // `index` lines all shorten together rather than each growing a knob.
    if let Some(n) = abbrev_len {
        let mut config = repo.config_snapshot_mut();
        config.append_config(Some(format!("core.abbrev={n}")), gix::config::Source::Cli)?;
        config.commit()?;
    }

    // Relative dates (`%cr`/`%ar`, `--date=relative`) are measured against now.
    let now = now_secs();

    // `cmd_log_init_finish()`: with no `--notes`/`--no-notes` of its own, a run
    // shows notes when the caller picked no format at all — or picked a user
    // format, where they surface only through `%N`. `--pretty=oneline` and the
    // other built-ins therefore stay silent unless asked.
    if !notes_opt.given && (!pretty_given || matches!(pretty, Pretty::User(_))) {
        notes_opt.enable_default();
    }
    let notes_trees = super::notes::load_display(&repo, &notes_opt)?;

    // git emits one terminated record per commit for any non-empty format, even
    // when a given commit expands to nothing (e.g. `%d` on an undecorated commit).
    // Only the genuinely empty user format (`--pretty=`, `tformat:`) emits nothing.
    let empty_user_format = matches!(&pretty, Pretty::User(f) if f.is_empty());

    // `--graph` needs every commit's block up front to lay out the columns, so it
    // buffers; every other format streams commit-by-commit (see the write below).
    let abbrev_cache = std::cell::RefCell::new(AbbrevCache::new(&repo));
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    // BLOCK-buffered, not line-buffered: Rust's stdout is a LineWriter, so writing
    // one terminated record per commit meant one write(2) per commit — 6375
    // syscalls for a full `log` on a deep history, which showed up as ~400ms of
    // system time against git's 8ms. git buffers and so does this now; the tail
    // is flushed below, and a closed pipe still surfaces as BrokenPipe.
    let mut stdout = std::io::BufWriter::with_capacity(64 * 1024, std::io::stdout().lock());
    let mut first = true;
    // Formats that need only what the walk produced skip the object read
    // entirely — the dominant cost of `--pretty=format:%H` on a deep history.
    let walk_only = match &pretty {
        Pretty::User(f) => !want_names && !emit_patch && format_is_walk_only(f),
        _ => false,
    };
    // `-p` renders each commit's patch from an immutable tree pair, so the patch
    // for a commit ten rows down the output does not depend on anything the rows
    // above it do. The window computes a batch of them across the thread pool
    // while the loop below stays a plain in-order stream — git computes them one
    // at a time on one core.
    let mut patches = PatchWindow::new(emit_patch, show_root, diff_merges);
    // Each record's text comes out of its own commit object, and reading 6000 of
    // them is the whole cost of a format like `--oneline` or `%s`. The window
    // renders a batch of records at a time across the thread pool; the loop below
    // still writes them one after another, in walk order.
    let mut entries = EntryWindow::new(EntryParams {
        abbrev_commit,
        show_parents,
        graph,
        children: children.as_ref(),
        date_mode,
        want_color,
        colors: &deco_colors,
        now,
        decorations: decorations.as_ref(),
        decorate,
        source_mode,
        mailmap: mailmap.as_ref(),
        terminator,
        empty_user_format,
        pretty: &pretty,
        notes: &notes_trees,
    });
    for (ni, node) in nodes.iter().enumerate() {
        if walk_only {
            let Pretty::User(fmt) = &pretty else { unreachable!() };
            let mut block: Vec<u8> = Vec::new();
            expand_walk_only(&mut block, fmt, node, abbrev_commit, &abbrev_cache, &repo);
            if terminator && !empty_user_format {
                block.push(b'\n');
            }
            if graph {
                blocks.push(block);
                continue;
            }
            let mut piece: Vec<u8> = Vec::new();
            if !terminator && !first {
                piece.push(b'\n');
            }
            piece.extend_from_slice(&block);
            first = false;
            if let Err(e) = stdout.write_all(&piece) {
                if e.kind() == std::io::ErrorKind::BrokenPipe {
                    crate::sigpipe::exit_broken_pipe();
                }
                return Err(e.into());
            }
            continue;
        }
        let mut block = entries.get(&repo, &nodes, ni, &abbrev_cache)?;

        // `log_tree_diff` short-circuits under `-L`: it flushes the pairs
        // `line_log_queue_pairs()` produced and returns, so neither the merge rule
        // nor `log.showRoot` applies — a surviving merge simply has no pairs, and a
        // root commit's creation pair is shown like any other.
        if line_level {
            let pairs = line_log_pairs.get(&node.id).map(Vec::as_slice).unwrap_or(&[]);
            let mut diff: Vec<u8> = Vec::new();
            if name_status {
                for (pair, _) in pairs {
                    let (status, path) = line_log_name_status(pair);
                    diff.push(status);
                    diff.push(b'\t');
                    diff.extend_from_slice(path);
                    diff.push(b'\n');
                }
            } else if name_only {
                for (pair, _) in pairs {
                    diff.extend_from_slice(&pair.path);
                    diff.push(b'\n');
                }
            } else if emit_patch && !pairs.is_empty() {
                diff = super::diff::line_range_patch(&repo, pairs, 3)?;
            }
            if !diff.is_empty() {
                // A merge's combined diff is separated from the header even under
                // `oneline`, which is the one format that otherwise runs the patch
                // straight on: `show_combined_diff()` writes the blank line itself.
                let combined_here = node.parents.len() > 1
                    && matches!(diff_merges, DiffMerges::Combined | DiffMerges::DenseCombined);
                if (!matches!(pretty, Pretty::Oneline) || combined_here) && !block.is_empty() {
                    block.push(b'\n');
                }
                block.extend_from_slice(&diff);
            }
        }
        // A root commit's diff (against the empty tree) is only shown when
        // `show_root` is set — git's `log.showRoot` (default true), forced on by
        // `--root`. Non-root commits are unaffected.
        // A merge reaches the count/name formats too once a `--diff-merges` mode is
        // in force; git diffs it against its first parent for those
        // (`log -c --stat` on a merge prints the first-parent stat).
        else if (want_names || emit_patch)
            && (node.parents.len() < 2 || diff_merges != DiffMerges::Off)
            && (show_root || !node.parents.is_empty())
        {
            let mut diff: Vec<u8> = Vec::new();
            if want_names {
                // `--name-only`/`--name-status` are the reported format when
                // present; git suppresses the count formats in that case, so the
                // blob reads they need are skipped too.
                let count_formats = (stat || numstat || shortstat) && !name_only && !name_status;
                // The record was rendered by a worker, which kept nothing; the
                // count formats need the commit itself for its tree.
                let commit = repo.find_object(node.id)?.try_into_commit()?;
                let mut files =
                    collect_changes(&repo, &commit, node.parents.first().copied(), count_formats)?;
                // `-- <pathspec>` limits what the name/stat formats report, not
                // just which commits reach them.
                // `--follow` limits each commit by the name the file had there.
                let limit: Vec<String> = match &node.follow_path {
                    Some(path) => vec![path.to_string()],
                    None => pathspecs.clone(),
                };
                if !limit.is_empty() {
                    let mut kept = Vec::with_capacity(files.len());
                    for f in files {
                        let mut hit = false;
                        for spec in &limit {
                            if pathspec_matches(spec, &f.path)? {
                                hit = true;
                                break;
                            }
                        }
                        if hit {
                            kept.push(f);
                        }
                    }
                    files = kept;
                }
                // A merge under a combined mode reports the *combined* pair list
                // here, with one status letter per parent — `show_raw_diff()` on a
                // `combine_diff_path`. The stat formats below stay on the
                // first-parent diff, which is where git leaves them.
                let combined_names = (node.parents.len() > 1
                    && matches!(diff_merges, DiffMerges::Combined | DiffMerges::DenseCombined)
                    && (name_only || name_status))
                    .then(|| {
                        super::diff::merge_combined_names(&repo, node.id, &node.parents, &pathspecs)
                    })
                    .transpose()?;
                if let Some(rows) = combined_names {
                    for (path, letters) in &rows {
                        if name_status {
                            diff.extend_from_slice(letters.as_bytes());
                            diff.push(b'\t');
                        }
                        diff.extend_from_slice(path);
                        diff.push(b'\n');
                    }
                } else if name_status {
                    for f in &files {
                        diff.push(f.status);
                        diff.push(b'\t');
                        diff.extend_from_slice(&f.path);
                        diff.push(b'\n');
                    }
                } else if name_only {
                    for f in &files {
                        diff.extend_from_slice(&f.path);
                        diff.push(b'\n');
                    }
                } else {
                    // git stacks the count formats in a fixed order: numstat, then
                    // the full stat block, then a bare shortstat summary if stat did
                    // not already print one.
                    if numstat {
                        emit_numstat(&mut diff, &files);
                    }
                    if stat {
                        emit_stat(&mut diff, &files, &stat_widths)?;
                    } else if shortstat {
                        emit_shortstat(&mut diff, &files)?;
                    }
                }
            }
            if emit_patch {
                // The full patch, rendered by the same pipeline as `git diff` so
                // the two agree byte-for-byte. git separates a preceding count
                // format from the patch with a blank line.
                // Under `--follow` the limit is the name the file had *at this
                // commit*, not the one on the command line — and it differs from
                // commit to commit, so the batching window (one pathspec per fill)
                // cannot serve it.
                let follow_patch: Vec<u8> = match &node.follow_path {
                    Some(path) => super::diff::commit_patches(
                        &repo,
                        &[(node.id, node.parents.first().copied())],
                        3,
                        &[path.to_string()],
                    )?
                    .pop()
                    .unwrap_or_default(),
                    None => Vec::new(),
                };
                let p: &[u8] = match &node.follow_path {
                    Some(_) => &follow_patch,
                    None => patches.get(&repo, &nodes, ni, 3, &pathspecs)?,
                };
                if !p.is_empty() {
                    if !diff.is_empty() {
                        diff.push(b'\n');
                    }
                    diff.extend_from_slice(p);
                }
            }
            if !diff.is_empty() {
                // git puts a separator between the log message and the diff for
                // every format but `oneline` — and only when the message block
                // rendered something to separate from. A `--stat` block shown
                // together with `-p` is fenced off with a `---` line; every other
                // diff format uses a plain blank line.
                // A merge's combined diff is separated from the header even under
                // `oneline`, which is the one format that otherwise runs the patch
                // straight on: `show_combined_diff()` writes the blank line itself.
                let combined_here = node.parents.len() > 1
                    && matches!(diff_merges, DiffMerges::Combined | DiffMerges::DenseCombined);
                if (!matches!(pretty, Pretty::Oneline) || combined_here) && !block.is_empty() {
                    if stat && emit_patch {
                        block.extend_from_slice(b"---\n");
                    } else {
                        block.push(b'\n');
                    }
                }
                block.extend_from_slice(&diff);
            }
        }
        if graph {
            // Buffer for the column layout, which spans all commits at once.
            blocks.push(block);
            continue;
        }

        // Stream this commit's block immediately, so `git log -p | head` stops
        // after a commit or two instead of computing every patch first. A
        // `format:`/built-in (separator) format precedes every record but the
        // first with a blank line; a `tformat:` record was already terminated
        // above, so no separator is inserted.
        let mut piece: Vec<u8> = Vec::new();
        if !terminator && !first {
            piece.push(b'\n');
        }
        piece.extend_from_slice(&block);
        first = false;
        // Each block ends in a newline, so the line-buffered stdout flushes it here;
        // a closed downstream pipe (`| head`) surfaces as a BrokenPipe on this write,
        // which is a normal stop rather than an error. No per-commit flush is needed.
        if let Err(e) = stdout.write_all(&piece) {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                crate::sigpipe::exit_broken_pipe();
            }
            return Err(e.into());
        }
    }

    // Persist whatever abbreviations this run had to compute, off the critical
    // path — the next `log` in any clone holding these objects reads them back.
    abbrev_cache.into_inner().flush();

    if graph {
        // `format:` separates records with a newline; `tformat:` already
        // terminated each block above.
        if !terminator {
            let last = blocks.len().saturating_sub(1);
            for (idx, block) in blocks.iter_mut().enumerate() {
                if idx != last {
                    block.push(b'\n');
                }
            }
        }
        let out = render_graph(&nodes, &blocks, graph_colors(&repo), want_color)?;
        match stdout.write_all(&out) {
            Ok(()) => Ok(ExitCode::SUCCESS),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                crate::sigpipe::exit_broken_pipe()
            }
            Err(e) => Err(e.into()),
        }
    } else {
        // Flush the tail: a block that did not end in a newline (an empty user
        // format) may still be buffered.
        match stdout.flush() {
            Ok(()) => Ok(ExitCode::SUCCESS),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                crate::sigpipe::exit_broken_pipe()
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// `line_log_rewrite_one`: replace a parent that `-L` dropped by the first commit
/// below it that is worth drawing an edge to. The walk stops at a merge, at an
/// excluded (`^rev`) commit, and at a commit `-L` kept; running out of parents
/// removes the edge entirely (git's `rewrite_one_noparents`).
fn line_log_rewrite_one(
    parent: ObjectId,
    seen: &HashMap<ObjectId, (Vec<ObjectId>, bool)>,
    hidden: &HashSet<ObjectId>,
) -> Option<ObjectId> {
    let mut p = parent;
    loop {
        let Some((parents, kept)) = seen.get(&p) else {
            return Some(p);
        };
        if parents.len() > 1 || hidden.contains(&p) || *kept {
            return Some(p);
        }
        p = *parents.first()?;
    }
}

/// `remove_duplicate_parents()`: after rewriting, a parent reachable from another
/// parent adds nothing to the simplified ancestry, and git drops it — which is
/// what turns a merge whose two sides collapse onto one line back into an
/// ordinary commit (no `Merge:` header, no fork in the graph).
fn prune_redundant_parents(repo: &gix::Repository, parents: &mut Vec<ObjectId>) {
    if parents.len() < 2 {
        return;
    }
    let original = parents.clone();
    parents.retain(|p| {
        !original.iter().any(|other| {
            other != p
                && repo
                    .merge_base(*p, *other)
                    .map(|base| base.detach() == *p)
                    .unwrap_or(false)
        })
    });
}

/// `rewrite_one()` for pathspec simplification: walk past every simplified-away
/// ancestor until a shown commit (or one the walk never reached) is found, so
/// `--graph`/`--parents` draw the simplified history rather than the real one.
fn simplify_rewrite_one(
    parent: ObjectId,
    simplified: &HashMap<ObjectId, (Vec<ObjectId>, bool)>,
) -> Option<ObjectId> {
    let mut p = parent;
    loop {
        let Some((parents, shown)) = simplified.get(&p) else {
            return Some(p);
        };
        if *shown {
            return Some(p);
        }
        p = *parents.first()?;
    }
}

/// The `--name-status` letter and path of a `-L` file pair.
///
/// `diff_resolve_rename_copy()` re-derives the letter from the two filespecs of the
/// `diff_filepair_dup()` the `-L` queue holds, and that copy carries no rename flag —
/// so even a pair whose sides name different files reports a plain `M`.
/// `diff_flush_raw()` then prints the pre-image path for anything but `R`/`C`.
fn line_log_name_status(pair: &line_log::Pair) -> (u8, &gix::bstr::BString) {
    match (pair.old, pair.new) {
        (None, _) => (b'A', &pair.path),
        (_, None) => (b'D', &pair.old_path),
        _ => (b'M', &pair.old_path),
    }
}

/// Parse a `-n`/`--max-count` value the way git does: a base-10 signed integer
/// with no trailing garbage. A negative value means "unlimited" (git's `-1`
/// sentinel), reported as `Ok(None)`; a non-negative value caps the walk.
/// `Err(())` marks a value git rejects with `fatal: '<value>': not an integer`.
fn parse_max_count(value: &str) -> Result<Option<usize>, ()> {
    match parse_int(value) {
        Some(n) if n < 0 => Ok(None),
        Some(n) => Ok(Some(n as usize)),
        None => Err(()),
    }
}

/// A non-negative base-10 integer (`--skip`, `--min-parents`, `--max-parents`).
/// `None` for anything git would reject with `fatal: '<value>': not an integer`.
fn parse_nonneg(value: &str) -> Option<usize> {
    match parse_int(value) {
        Some(n) if n >= 0 => Some(n as usize),
        _ => None,
    }
}

/// A base-10 signed integer git would accept: optional `+`/`-`, then digits only,
/// no trailing characters, no overflow. Returns `None` for anything else.
fn parse_int(value: &str) -> Option<i64> {
    let (neg, digits) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = digits.parse().ok()?;
    Some(if neg { -n } else { n })
}

/// Whether `spec` names a path, so git treats an unresolvable revision token as a
/// pathspec instead of erroring (`git checkout`-style disambiguation, git's
/// `verify_filename`). True when the path is present in the working tree, or is
/// tracked in the index — the latter covers `git log <file>` for a path that was
/// deleted from the worktree but still has history.
fn spec_is_path(repo: &gix::Repository, spec: &str) -> bool {
    if std::path::Path::new(spec).exists() {
        return true;
    }
    let needle = spec.strip_suffix('/').unwrap_or(spec);
    if needle.is_empty() {
        return false;
    }
    let Ok(index) = repo.open_index() else {
        return false;
    };
    let n = needle.as_bytes();
    index.entries().iter().any(|e| {
        let p: &[u8] = e.path(&index).as_ref();
        // Exact file, or a directory prefix (`p` lies under `needle/`).
        p == n || (p.len() > n.len() && p.starts_with(n) && p[n.len()] == b'/')
    })
}

/// git distinguishes a well-formed but absent object id from an unresolvable name:
/// the former is a "bad object", the latter an "ambiguous argument".
fn bad_revision_message(spec: &str, hex_len: usize) -> String {
    if spec.len() == hex_len && spec.bytes().all(|b| b.is_ascii_hexdigit()) {
        format!("fatal: bad object {spec}\n")
    } else {
        format!(
            "fatal: ambiguous argument '{spec}': unknown revision or path not in the working tree.\n\
             Use '--' to separate paths from revisions, like this:\n\
             'git <command> [<revision>...] -- [<file>...]'\n"
        )
    }
}

// ---------------------------------------------------------------------------
// Revision walk
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    /// git's default: pure commit-date order.
    Default,
    /// `--date-order`: topological, breaking ties by commit date.
    Date,
    /// `--topo-order`: topological, following the graph rather than the clock.
    Topo,
}

/// What the walk needs to know about a commit, read once up front.
/// Everything a commit's record needs beyond the commit itself: the flags and
/// lookup tables that are fixed for the whole command.
struct EntryParams<'a> {
    abbrev_commit: bool,
    show_parents: bool,
    /// `--graph`: suppresses the `--boundary` mark, which the `o` node draws instead
    /// (`show_log` skips `put_revision_mark` whenever a graph is active).
    graph: bool,
    /// `--children`: each commit`s children among the walked set, or `None` when
    /// the flag is off.
    children: Option<&'a HashMap<ObjectId, Vec<ObjectId>>>,
    date_mode: DateMode,
    want_color: bool,
    /// The resolved `color.decorate.*` / `color.diff.commit` slots.
    colors: &'a super::color::DecorateColors,
    now: i64,
    decorations: Option<&'a Decorations>,
    decorate: DecorateStyle,
    source_mode: bool,
    /// `--use-mailmap` / `log.mailmap`: the loaded mailmap, or `None` when the
    /// identities are shown as recorded.
    mailmap: Option<&'a Mailmap>,
    terminator: bool,
    empty_user_format: bool,
    pretty: &'a Pretty,
    /// The notes trees to render after the message; empty when notes are off.
    notes: &'a [super::notes::Tree],
}

/// A look-ahead buffer of rendered commit records.
///
/// Reading a commit object and expanding its format is per-commit work with no
/// shared state, and on a deep history it is the entire cost of `--oneline`,
/// `%s` or the default format — the walk itself is already cheap. The window
/// renders `SPAN` records at a time across the thread pool and hands them out in
/// order, so the caller stays a simple in-order loop and memory is bounded by
/// the span, not the history.
struct EntryWindow<'a> {
    params: EntryParams<'a>,
    /// Index of `slots[0]` within the caller's node list.
    start: usize,
    slots: Vec<Vec<u8>>,
}

impl<'a> EntryWindow<'a> {
    /// Records rendered per refill. Records are small (a line for `--oneline`,
    /// a paragraph for the default format), so the span can be wide.
    const SPAN: usize = 256;
    /// Records per worker. An object read plus a format expansion is small work,
    /// so a batch must be sizeable before threads repay their setup.
    const PER_WORKER: usize = 32;

    fn new(params: EntryParams<'a>) -> Self {
        EntryWindow { params, start: 0, slots: Vec::new() }
    }

    /// The rendered record for `nodes[i]`, refilling the window when `i` runs
    /// past it. The record is moved out: the caller appends its diff to it.
    fn get(
        &mut self,
        repo: &gix::Repository,
        nodes: &[Node],
        i: usize,
        abbrev: &std::cell::RefCell<AbbrevCache>,
    ) -> Result<Vec<u8>> {
        if i < self.start || i >= self.start + self.slots.len() {
            let end = (i + Self::SPAN).min(nodes.len());
            self.slots = self.render_span(repo, &nodes[i..end], abbrev)?;
            self.start = i;
        }
        Ok(std::mem::take(&mut self.slots[i - self.start]))
    }

    fn render_span(
        &self,
        repo: &gix::Repository,
        span: &[Node],
        abbrev: &std::cell::RefCell<AbbrevCache>,
    ) -> Result<Vec<Vec<u8>>> {
        let workers = crate::threads::count(span.len(), Self::PER_WORKER);
        if workers <= 1 {
            return span.iter().map(|n| entry_block(repo, n, &self.params, abbrev)).collect();
        }

        let cursor = std::sync::atomic::AtomicUsize::new(0);
        let mut done: Vec<(usize, Vec<u8>)> = Vec::with_capacity(span.len());
        let mut caches: Vec<AbbrevCache> = Vec::with_capacity(workers);
        let mut failure: Option<anyhow::Error> = None;
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(workers);
            for _ in 0..workers {
                let proto = repo.clone();
                // A worker abbreviates ids of its own, so it takes a fork of the
                // cache — the ledger's half is shared, the new half is private
                // until it is merged back below.
                let mine_abbrev = std::cell::RefCell::new(abbrev.borrow().fork());
                let cursor = &cursor;
                let params = &self.params;
                #[allow(clippy::type_complexity)] // per-worker (rows, abbrev-cache) result
                handles.push(scope.spawn(move || -> Result<(Vec<(usize, Vec<u8>)>, AbbrevCache)> {
                    let repo = proto;
                    let mut mine = Vec::new();
                    loop {
                        let i = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(node) = span.get(i) else { break };
                        mine.push((i, entry_block(&repo, node, params, &mine_abbrev)?));
                    }
                    Ok((mine, mine_abbrev.into_inner()))
                }));
            }
            for h in handles {
                match h.join() {
                    Ok(Ok((mine, cache))) => {
                        done.extend(mine);
                        caches.push(cache);
                    }
                    Ok(Err(e)) => {
                        failure.get_or_insert(e);
                    }
                    Err(_) => {
                        failure.get_or_insert_with(|| anyhow::anyhow!("log worker panicked"));
                    }
                }
            }
        });
        if let Some(e) = failure {
            return Err(e);
        }
        for cache in caches {
            abbrev.borrow_mut().absorb(cache);
        }

        done.sort_by_key(|(i, _)| *i);
        Ok(done.into_iter().map(|(_, block)| block).collect())
    }
}

/// One commit's rendered record: its header/message block plus the record
/// terminator, with no diff attached.
fn entry_block(
    repo: &gix::Repository,
    node: &Node,
    p: &EntryParams<'_>,
    abbrev: &std::cell::RefCell<AbbrevCache>,
) -> Result<Vec<u8>> {
    let commit = repo.find_object(node.id)?.try_into_commit()?;
    // `--parents` then `--children` decorate the header with ids, in that order
    // (`show_log` prints `print_parents` before `children`). A child list is what
    // the walk saw, so it names only commits this run reached.
    let mut extra = Vec::new();
    let push_ids = |ids: &[ObjectId], out: &mut Vec<u8>| {
        for id in ids {
            out.push(b' ');
            let attached = id.attach(repo);
            if p.abbrev_commit {
                out.extend_from_slice(abbrev.borrow_mut().get(attached).as_bytes());
            } else {
                out.extend_from_slice(attached.to_string().as_bytes());
            }
        }
    };
    if p.show_parents {
        push_ids(&node.parents, &mut extra);
    }
    if let Some(children) = p.children {
        push_ids(children.get(&node.id).map_or(&[][..], Vec::as_slice), &mut extra);
    }
    let ctx = RenderCtx {
        abbrev_commit: p.abbrev_commit,
        abbrev,
        date_mode: p.date_mode,
        extra,
        want_color: p.want_color,
        colors: p.colors,
        now: p.now,
        decorations: p.decorations,
        decorate: p.decorate,
        source: if p.source_mode { Some(node.source.as_bytes()) } else { None },
        mailmap: p.mailmap,
        notes: p.notes,
        repo,
        mark: if node.boundary && !p.graph { "- " } else { "" },
        parents: &node.parents,
    };
    let mut block: Vec<u8> = Vec::new();
    render_entry(&mut block, &commit, p.pretty, &ctx)?;
    // A `tformat:` record is terminated by a newline. git still terminates a
    // record whose expansion happened to be empty (so `%d` prints one line per
    // commit); only the genuinely empty user format emits no terminator.
    if p.terminator && !p.empty_user_format {
        block.push(b'\n');
    }
    Ok(block)
}

/// A look-ahead buffer of rendered `-p` patch bodies.
///
/// The output is a stream, but the work behind it is not sequential: a commit's
/// patch is a pure function of its tree and its first parent's, both immutable.
/// So instead of computing one patch, printing it, and leaving the rest of the
/// machine idle — which is all git can do — the window computes the next
/// `SPAN` commits' patches at once across the thread pool and hands them out in
/// order. Memory stays bounded by the span rather than by the length of the
/// history, so `log -p` over ten thousand commits still streams.
///
/// Commits the caller will not show a diff for (merges, and root commits under
/// `log.showRoot=false`) get an empty slot rather than a wasted diff.
struct PatchWindow {
    active: bool,
    show_root: bool,
    /// `--diff-merges=<mode>`: what a merge commit's patch shows.
    merges: DiffMerges,
    /// Index of `slots[0]` within the caller's node list.
    start: usize,
    slots: Vec<Vec<u8>>,
}

impl PatchWindow {
    /// Commits computed per refill. Large enough to keep every core busy on a
    /// wide box, small enough that the buffered patches stay a few megabytes.
    const SPAN: usize = 64;


    fn new(active: bool, show_root: bool, merges: DiffMerges) -> Self {
        PatchWindow { active, show_root, merges, start: 0, slots: Vec::new() }
    }

    /// `true` when git renders a diff for this commit at all: a merge only under a
    /// `--diff-merges` mode other than `off` (which `-m`/`-c`/`--cc` select), and a
    /// root commit's diff obeys `log.showRoot`.
    fn diffable(&self, node: &Node) -> bool {
        if node.parents.len() > 1 {
            return self.merges != DiffMerges::Off;
        }
        self.show_root || !node.parents.is_empty()
    }

    /// The patch body for `nodes[i]`, refilling the window when `i` runs past it.
    ///
    /// A merge is rendered here rather than through the batch: its shape depends on
    /// the `--diff-merges` mode, and the combined form needs every parent's tree at
    /// once.
    fn get<'a>(
        &'a mut self,
        repo: &gix::Repository,
        nodes: &[Node],
        i: usize,
        ctx: u32,
        paths: &[String],
    ) -> Result<&'a [u8]> {
        if !self.active {
            return Ok(&[]);
        }
        if i < self.start || i >= self.start + self.slots.len() {
            let end = (i + Self::SPAN).min(nodes.len());
            let span = &nodes[i..end];
            // Only diffable commits become jobs; `at[k]` is the slot that job
            // `k`'s result belongs in, so the batch carries no wasted diffs.
            let mut jobs: Vec<(ObjectId, Option<ObjectId>)> = Vec::with_capacity(span.len());
            let mut at: Vec<usize> = Vec::with_capacity(span.len());
            // A merge under `combined`/`dense-combined` needs every parent tree at
            // once, so it is rendered on its own rather than as a two-way job.
            let mut merged: Vec<(usize, Vec<u8>)> = Vec::new();
            for (k, n) in span.iter().enumerate() {
                if !self.diffable(n) {
                    continue;
                }
                if n.parents.len() > 1 {
                    match self.merges {
                        DiffMerges::Combined | DiffMerges::DenseCombined => {
                            merged.push((
                                k,
                                super::diff::merge_combined_patch(
                                    repo,
                                    n.id,
                                    &n.parents,
                                    paths,
                                    ctx,
                                    self.merges == DiffMerges::DenseCombined,
                                )?,
                            ));
                            continue;
                        }
                        // `separate` repeats the *record* once per parent, each
                        // headed `<oid> (from <parent-oid>) <subject>` — the insert
                        // lives inside `show_log()`'s header, which is rendered
                        // before any patch is known here. Emitting the patches
                        // without it would print one header for N diffs, so this
                        // stops instead.
                        DiffMerges::Separate => {
                            anyhow::bail!(
                                "`-m` with a patch format is not ported: git repeats the commit \
                                 header once per parent with its `(from <oid>)` insert, which this \
                                 renderer produces before the per-parent diffs exist"
                            );
                        }
                        // `first-parent` is an ordinary two-way job.
                        DiffMerges::FirstParent | DiffMerges::Off => {}
                    }
                }
                jobs.push((n.id, n.parents.first().copied()));
                at.push(k);
            }
            let computed = super::diff::commit_patches(repo, &jobs, ctx, paths)?;
            self.slots = vec![Vec::new(); span.len()];
            for (slot, patch) in at.into_iter().zip(computed) {
                self.slots[slot] = patch;
            }
            for (slot, patch) in merged {
                self.slots[slot] = patch;
            }
            self.start = i;
        }
        Ok(&self.slots[i - self.start])
    }
}

/// `--diff-merges=<mode>` (diff-merges.c): what a merge commit's patch shows.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum DiffMerges {
    /// The default: a merge gets no patch.
    Off,
    /// `-m` / `--diff-merges=separate`: one ordinary patch per parent, each
    /// preceded by its own copy of the commit header.
    Separate,
    /// `-c` / `--diff-merges=combined`: one `diff --combined` section per path
    /// that differs from every parent.
    Combined,
    /// `--cc` / `--diff-merges=dense-combined`: the same, headed `diff --cc`.
    DenseCombined,
    /// `--diff-merges=first-parent`: an ordinary patch against the first parent.
    FirstParent,
}

impl DiffMerges {
    fn parse(v: &str) -> Option<Self> {
        Some(match v {
            "off" | "none" => DiffMerges::Off,
            "m" | "separate" => DiffMerges::Separate,
            "c" | "combined" => DiffMerges::Combined,
            "cc" | "dense-combined" => DiffMerges::DenseCombined,
            "1" | "first-parent" => DiffMerges::FirstParent,
            _ => return None,
        })
    }
}

pub(crate) struct Node {
    pub(crate) id: ObjectId,
    pub(crate) parents: Vec<ObjectId>,
    pub(crate) time: i64,
    /// `--source`: the ref/argument this commit was first reached from. Empty when
    /// `--source` is off (the field is never rendered in that case).
    pub(crate) source: String,
    /// Order this node entered the frontier, which is what breaks a date tie.
    /// Set by the walk at push time; never rendered.
    pub(crate) seq: u64,
    /// `--boundary`: an excluded commit that a shown commit descends from, which
    /// git prints with a `-` mark after the rest of the walk.
    pub(crate) boundary: bool,
    /// `--follow`: the name the tracked file had *at this commit*, which is what
    /// its diff and name formats are limited to. `None` when not following.
    pub(crate) follow_path: Option<gix::bstr::BString>,
}

/// Heap order for the walk's frontier: newest commit-date first, ties broken by
/// insertion order — the commit that entered the frontier first pops first.
///
/// git's frontier is a list kept sorted by `commit_list_insert_by_date()`, which
/// walks past every entry whose date is *not older* than the new one before
/// splicing it in. Equal dates therefore come out first-in-first-out, and equal
/// dates are the norm rather than the exception: an import, a scripted series,
/// or any two commits inside the same second all tie. Breaking those ties by
/// object id instead reorders `git log` against git — and against this port's
/// own `rev-list`, which goes through gitoxide's date-ordered walk.
impl Ord for Node {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.time.cmp(&other.time).then_with(|| other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time && self.seq == other.seq
    }
}
impl Eq for Node {}

/// Abbreviated object ids, memoised in the zero-copy cache.
///
/// `gix`'s `shorten_or_id()` disambiguates a prefix against the whole object
/// database on EVERY call, which measures ~60-90us per id here regardless of
/// repository size — on a 6375-commit `log --oneline` that alone is ~480ms of
/// the 530ms runtime (`%H` renders in 50ms, `%h` in 534ms; the delta is nothing
/// but abbreviation). git pays a couple of binary searches for the same answer.
///
/// The answer is a pure function of an immutable object id and the repository's
/// current hex length, so it is cached machine-wide, keyed by both. Object ids
/// are content addresses, so an entry is valid for every clone that holds the
/// object; the hex length is part of the key because the correct abbreviation
/// grows as a repository does, and a stale, now-ambiguous prefix must never be
/// served.
///
/// Nothing is loaded up front. The store is an mmap'd image searched per id
/// (`crate::rcache`), so a `log -5` touches a handful of pages instead of
/// decoding every abbreviation the machine has ever computed — which is what
/// reading them out of the SQLite ledger cost, hex-parsing each id on the way in.
struct AbbrevCache {
    hex_len: usize,
    /// Abbreviations computed by THIS cache. Anything else is one lookup away in
    /// the shared image, so only what the image lacks is held here.
    local: std::collections::HashMap<ObjectId, String>,
    /// New rows for the cache, keyed by the id's raw bytes as the image keys them.
    fresh: Vec<(Vec<u8>, String)>,
}

impl AbbrevCache {
    fn new(repo: &gix::Repository) -> Self {
        // The repo-wide length: `core.abbrev` when set, else gix's auto rule.
        let hex_len = repo
            .config_snapshot()
            .integer("core.abbrev")
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(0);
        AbbrevCache { hex_len, local: Default::default(), fresh: Vec::new() }
    }

    /// A cache for a worker thread: the shared image needs no handing over, and
    /// anything the worker computes stays private until
    /// [`absorb`](Self::absorb) takes it.
    fn fork(&self) -> Self {
        AbbrevCache { hex_len: self.hex_len, local: Default::default(), fresh: Vec::new() }
    }

    /// Take what a forked cache computed. Two workers may have shortened the same
    /// id, which is harmless — the cache write is keyed by id, so a duplicate row
    /// overwrites itself with the same value.
    fn absorb(&mut self, other: Self) {
        self.local.extend(other.local);
        self.fresh.extend(other.fresh);
    }

    fn get(&mut self, id: gix::Id<'_>) -> String {
        let oid = id.detach();
        if let Some(short) = self.local.get(&oid) {
            return short.clone();
        }
        if let Some(short) = crate::rcache::abbrev_load(oid.as_slice(), self.hex_len) {
            return short.to_string();
        }
        let short = id.shorten_or_id().to_string();
        self.fresh.push((oid.as_slice().to_vec(), short.clone()));
        self.local.insert(oid, short.clone());
        short
    }

    /// Hand what this run computed to the cache's writer thread.
    ///
    /// The rows are queued rather than written here: the command has its
    /// abbreviations already, and `run()` waits for the queue once, after the
    /// output is on its way. Losing a batch would only cost a recomputation, so
    /// nothing here reports an error.
    fn flush(self) {
        if self.fresh.is_empty() {
            return;
        }
        crate::rcache::cache_write(crate::rcache::CacheWrite::Abbrev {
            hex_len: self.hex_len,
            rows: self.fresh,
        });
    }
}

/// The byte two hex digits name, or `None` if either is not a hex digit. Upper
/// and lower case both count, as in git.
fn hex_byte(hi: Option<char>, lo: Option<char>) -> Option<u8> {
    let nibble = |c: Option<char>| c?.to_digit(16).map(|v| v as u8);
    Some((nibble(hi)? << 4) | nibble(lo)?)
}

/// Whether a user format can be rendered from the WALK alone — the ids and
/// parents already in hand — without reading each commit object.
///
/// git's `%H`/`%h`/`%P`/`%p` need nothing the walk did not already produce, and
/// on a deep history the object read is the whole cost: rendering `%H` for 6375
/// commits spent ~40ms opening objects for data it never used, which is why
/// zvcs's `%H` was slower than its own `--oneline`.
///
/// Anything else — a date, an author, a message, a decoration, a colour — still
/// takes the object, so the check is a deliberate whitelist: an unknown
/// placeholder answers `false` and keeps the faithful path.
fn format_is_walk_only(fmt: &str) -> bool {
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '%' {
            i += 1;
            continue;
        }
        let Some(&p) = chars.get(i + 1) else { return false };
        match p {
            'H' | 'h' | 'P' | 'p' | 'n' | '%' => i += 2,
            _ => return false,
        }
    }
    true
}

/// Expand a walk-only format for one node. Mirrors the placeholder handling in
/// [`expand_format`] for exactly the subset [`format_is_walk_only`] admits.
fn expand_walk_only(
    out: &mut Vec<u8>,
    fmt: &str,
    node: &Node,
    abbrev_commit: bool,
    cache: &std::cell::RefCell<AbbrevCache>,
    repo: &gix::Repository,
) {
    let short = |id: ObjectId| -> String {
        if abbrev_commit || true {
            cache.borrow_mut().get(id.attach(repo))
        } else {
            id.to_string()
        }
    };
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '%' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(chars[i].encode_utf8(&mut buf).as_bytes());
            i += 1;
            continue;
        }
        match chars.get(i + 1) {
            Some('H') => out.extend_from_slice(node.id.to_string().as_bytes()),
            Some('h') => out.extend_from_slice(short(node.id).as_bytes()),
            Some('P') => {
                let text: Vec<String> = node.parents.iter().map(ToString::to_string).collect();
                out.extend_from_slice(text.join(" ").as_bytes());
            }
            Some('p') => {
                let text: Vec<String> = node.parents.iter().map(|p| short(*p)).collect();
                out.extend_from_slice(text.join(" ").as_bytes());
            }
            Some('n') => out.push(b'\n'),
            Some('%') => out.push(b'%'),
            _ => {}
        }
        i += 2;
    }
}

/// Resolve a single revision to its object id (a range endpoint), `Err(())` if it
/// doesn't name anything — the caller turns that into git's bad-revision error.
fn resolve_rev(repo: &gix::Repository, spec: &str) -> Result<ObjectId, ()> {
    repo.rev_parse_single(spec).map(|id| peel_to_commit(repo, id.detach())).map_err(|_| ())
}

/// The commit a revision names, following annotated tags.
///
/// `git log v1.0` walks from the COMMIT the tag points at; the tag object itself
/// is not a walkable node. Without this, every release tag — the most natural
/// thing to `git log` — failed with "was supposed to be of kind commit, but was
/// kind tag". A spec that names something with no commit behind it (a tree, a
/// blob) is left as-is so the walk reports it the way git does.
fn peel_to_commit(repo: &gix::Repository, id: ObjectId) -> ObjectId {
    repo.find_object(id)
        .ok()
        .and_then(|obj| obj.peel_tags_to_end().ok())
        .filter(|obj| obj.kind == gix::object::Kind::Commit)
        .map_or(id, |obj| obj.id)
}

/// Every commit reachable from `roots` (inclusive) — the "uninteresting" set for a
/// `..` exclusion, gathered by a plain ancestor DFS.
fn ancestor_closure(repo: &gix::Repository, roots: &[ObjectId]) -> Result<HashSet<ObjectId>> {
    let mut set: HashSet<ObjectId> = HashSet::new();
    let mut stack: Vec<ObjectId> = Vec::new();
    for &r in roots {
        if set.insert(r) {
            stack.push(r);
        }
    }
    while let Some(id) = stack.pop() {
        let Ok(obj) = repo.find_object(id) else { continue };
        let Ok(commit) = obj.try_into_commit() else { continue };
        for p in commit.parent_ids() {
            let pid = p.detach();
            if set.insert(pid) {
                stack.push(pid);
            }
        }
    }
    Ok(set)
}

fn read_node(repo: &gix::Repository, id: ObjectId) -> Result<Node> {
    let commit = repo.find_object(id)?.try_into_commit()?;
    Ok(Node {
        id,
        parents: commit.parent_ids().map(|p| p.detach()).collect(),
        time: commit.time()?.seconds,
        seq: 0,
        boundary: false,
        follow_path: None,
        source: String::new(),
    })
}

/// The walk needs exactly three things per commit — id, parents, commit time —
/// and all three live in the **commit-graph** when the repository has one, which
/// is why git can walk a 6000-commit history without touching the object
/// database. `read_node` decodes a full commit object (zlib inflate, header
/// parse) for the same three fields.
///
/// This reader prefers the graph and falls back to the object for any commit the
/// graph does not carry (a graph written before the newest commits, or none at
/// all), so the walk is always correct and merely faster when the graph is
/// current.
struct NodeReader {
    graph: Option<gix::commitgraph::Graph>,
}

impl NodeReader {
    fn new(repo: &gix::Repository) -> Self {
        NodeReader { graph: repo.commit_graph().ok() }
    }

    fn read(&self, repo: &gix::Repository, id: ObjectId) -> Result<Node> {
        if let Some(graph) = &self.graph {
            if let Some(commit) = graph.commit_by_id(id) {
                let parents: Vec<ObjectId> = commit
                    .iter_parents()
                    .filter_map(|p| p.ok())
                    .map(|pos| graph.commit_at(pos).id().to_owned())
                    .collect();
                return Ok(Node {
                    id,
                    parents,
                    time: commit.committer_timestamp() as i64,
                    source: String::new(),
                    seq: 0,
                    boundary: false,
        follow_path: None,
                });
            }
        }
        read_node(repo, id)
    }
}

/// git's `commit_list_insert_by_date`: keep the list newest-first, and place a
/// commit *after* every commit with the same date so equal timestamps come out
/// in insertion order — the tie-break git's priority queue also uses.
#[allow(dead_code)] // faithful port of git's commit_list_insert_by_date; kept for the walk.
fn insert_by_date(list: &mut Vec<Node>, node: Node) {
    let pos = list
        .iter()
        .position(|e| e.time < node.time)
        .unwrap_or(list.len());
    list.insert(pos, node);
}

/// `--source` for a revision walk: commit → the tip name it was first reached
/// from, computed by the same walk `git log` uses so the inheritance rule
/// (`add_parents_to_list`: a parent takes its child's source) is identical.
///
/// `git show <range>` needs this because its own traversal only yields ids;
/// resolving the source separately here keeps one implementation of the rule.
pub(crate) fn source_map(
    repo: &gix::Repository,
    tips: &[ObjectId],
    tip_sources: &[String],
    first_parent: bool,
    exclude: &[ObjectId],
) -> Result<HashMap<ObjectId, String>> {
    let hidden = if exclude.is_empty() {
        HashSet::new()
    } else {
        ancestor_closure(repo, exclude)?
    };
    let nodes = walk(repo, tips, tip_sources, first_parent, &hidden, None)?;
    Ok(nodes.into_iter().map(|n| (n.id, n.source)).collect())
}

/// Breadth-first walk over the reachable history, newest commit first. With
/// `first_parent`, only the first parent of each commit is followed — git's
/// `--first-parent`.
fn walk(
    repo: &gix::Repository,
    tips: &[ObjectId],
    tip_sources: &[String],
    first_parent: bool,
    hidden: &HashSet<ObjectId>,
    budget: Option<usize>,
) -> Result<Vec<Node>> {
    // Shallow commits (from `.git/shallow`, as a `--depth` clone leaves) are grafted
    // to have no parents: the walk must stop at them, not try to read their absent
    // parent objects (which is git's `is_repository_shallow` / grafting behaviour).
    let shallow: HashSet<ObjectId> = repo
        .shallow_commits()
        .ok()
        .flatten()
        .map(|c| c.iter().copied().collect())
        .unwrap_or_default();

    // Pre-seeding `seen` with the hidden (uninteresting) commits means any tip or
    // parent reachable from a negative range endpoint is never emitted or traversed
    // — git's `..` exclusion, implemented as a boundary the walk cannot cross.
    let reader = NodeReader::new(repo);
    let mut seen: HashSet<ObjectId> = hidden.clone();
    // A binary heap, not a date-sorted Vec: the frontier is popped newest-first,
    // and both the push and the pop are logarithmic. The previous sorted-insert
    // plus `remove(0)` made a full walk quadratic in the number of commits.
    let mut pending: std::collections::BinaryHeap<Node> = std::collections::BinaryHeap::new();
    // Insertion order, which is what decides a date tie — see `Node`'s `Ord`.
    // Tips enter in argument order and parents in parent order, exactly as
    // `add_parents_to_list()` feeds git's frontier.
    let mut seq: u64 = 0;
    for (idx, tip) in tips.iter().enumerate() {
        if seen.insert(*tip) {
            let mut node = reader.read(repo, *tip)?;
            // `--source` names each tip; without it `tip_sources` is empty and the
            // source stays blank (never rendered). Parents inherit below.
            if let Some(src) = tip_sources.get(idx) {
                node.source = src.clone();
            }
            node.seq = seq;
            seq += 1;
            pending.push(node);
        }
    }

    let mut out: Vec<Node> = Vec::new();
    while let Some(node) = pending.pop() {
        // `budget` is `skip + max-count` when the caller has established that
        // nothing downstream can drop a commit (no pathspec, no parent/date/grep
        // filter, default order). Stopping there turns `log -n 100` on a
        // 6000-commit history from a full-history read into 100 object reads.
        if budget.is_some_and(|b| out.len() >= b) {
            break;
        }
        let parents: &[ObjectId] = if shallow.contains(&node.id) {
            &[] // grafted: a shallow commit's parents are outside the clone
        } else if first_parent {
            &node.parents[..node.parents.len().min(1)]
        } else {
            &node.parents
        };
        for parent in parents {
            if seen.insert(*parent) {
                let mut pnode = reader.read(repo, *parent)?;
                // git's `add_parents_to_list`: a parent inherits the source of the
                // commit that first reaches it (an empty-string clone when off).
                pnode.source = node.source.clone();
                pnode.seq = seq;
                seq += 1;
                pending.push(pnode);
            }
        }
        out.push(node);
    }
    Ok(out)
}

/// git's `sort_in_topological_order`: an indegree count over the already-walked
/// set, drained through a queue that is date-ordered for `--date-order` and a
/// LIFO stack for `--topo-order`.
pub(crate) fn topo_sort(nodes: Vec<Node>, by_date: bool) -> Vec<Node> {
    let mut indegree: std::collections::HashMap<ObjectId, usize> =
        nodes.iter().map(|n| (n.id, 1usize)).collect();
    for node in &nodes {
        for parent in &node.parents {
            if let Some(d) = indegree.get_mut(parent) {
                *d += 1;
            }
        }
    }

    let index: std::collections::HashMap<ObjectId, usize> =
        nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect();

    // Tips are queued in list order. A LIFO stack is reversed first so that
    // popping still yields them in that order, exactly as git does.
    let mut queue: Vec<usize> = (0..nodes.len())
        .filter(|&i| indegree.get(&nodes[i].id) == Some(&1))
        .collect();
    if !by_date {
        queue.reverse();
    }

    let mut out: Vec<usize> = Vec::with_capacity(nodes.len());
    while !queue.is_empty() {
        let at = if by_date {
            // Highest commit date wins; the earliest-queued entry breaks ties.
            let mut best = 0usize;
            for (k, &i) in queue.iter().enumerate() {
                if nodes[i].time > nodes[queue[best]].time {
                    best = k;
                }
            }
            best
        } else {
            queue.len() - 1
        };
        let i = queue.remove(at);

        for parent in &nodes[i].parents {
            if let Some(d) = indegree.get_mut(parent) {
                if *d == 0 {
                    continue;
                }
                *d -= 1;
                if *d == 1 {
                    if let Some(&pi) = index.get(parent) {
                        queue.push(pi);
                    }
                }
            }
        }
        out.push(i);
    }

    // Anything the drain could not reach keeps its original relative position.
    let mut placed: Vec<bool> = vec![false; nodes.len()];
    for &i in &out {
        placed[i] = true;
    }
    for (i, &is_placed) in placed.iter().enumerate() {
        if !is_placed {
            out.push(i);
        }
    }

    let mut slots: Vec<Option<Node>> = nodes.into_iter().map(Some).collect();
    out.into_iter()
        .filter_map(|i| slots[i].take())
        .collect()
}

// ---------------------------------------------------------------------------
// Pretty formats
// ---------------------------------------------------------------------------

pub(crate) enum Pretty {
    /// git's default: `commit`/`Merge`/`Author`/`Date` and an indented message.
    Medium,
    /// `medium` without the `Date` line, and only the subject.
    Short,
    /// `commit`/`Merge`/`Author`/`Commit` and the full indented message.
    Full,
    /// `full` plus `AuthorDate`/`CommitDate` lines.
    Fuller,
    /// The raw object header: `tree`/`parent`/`author`/`committer`.
    Raw,
    /// `<abbrev> (<subject>, <short-date>)` on one line.
    Reference,
    /// `<hash> <subject>` on one line.
    Oneline,
    /// A `--format=`/`format:` string with `%` placeholders.
    User(String),
}

/// git's `get_commit_format`, the shared parser behind `--pretty=` and
/// `--format=`. Returns the format and whether it terminates (rather than
/// separates) records:
///   * `Ok(Some(..))` — a valid, supported format.
///   * `Ok(None)`     — a value git itself rejects (`fatal: invalid --pretty
///     format: <arg>`, exit 128): non-empty, no `%`, not a `format:`/`tformat:`
///     prefix, and not a known format name.
///   * `Err(..)`      — a value git accepts but this port does not yet render
///     (an unsupported `%` placeholder), surfaced terse rather than faked.
///
/// An empty value is git's empty user format: it renders nothing per commit and,
/// as a terminator format, drops even the trailing newline.
pub(crate) fn get_commit_format(spec: &str) -> Result<Option<(Pretty, bool)>> {
    if spec.is_empty() {
        return Ok(Some((Pretty::User(String::new()), true)));
    }
    if let Some(fmt) = spec.strip_prefix("format:") {
        check_format(fmt)?;
        return Ok(Some((Pretty::User(fmt.to_string()), false)));
    }
    if let Some(fmt) = spec.strip_prefix("tformat:") {
        check_format(fmt)?;
        return Ok(Some((Pretty::User(fmt.to_string()), true)));
    }
    if spec.contains('%') {
        check_format(spec)?;
        return Ok(Some((Pretty::User(spec.to_string()), true)));
    }
    match spec {
        "oneline" => Ok(Some((Pretty::Oneline, true))),
        "medium" => Ok(Some((Pretty::Medium, false))),
        "short" => Ok(Some((Pretty::Short, false))),
        "full" => Ok(Some((Pretty::Full, false))),
        "fuller" => Ok(Some((Pretty::Fuller, false))),
        "raw" => Ok(Some((Pretty::Raw, false))),
        "reference" => Ok(Some((Pretty::Reference, true))),
        // `email`/`mboxrd` need the full mailbox/`From ` framing git's format-patch
        // machinery produces; surfaced terse rather than faked.
        "email" | "mboxrd" => {
            bail!("pretty format {spec:?} is not ported")
        }
        _ => Ok(None),
    }
}

/// Reject any placeholder [`expand_format`] does not implement, so an unsupported
/// format fails loudly instead of expanding to something plausible but wrong.
///
/// `%C` is always accepted: like git, an unrecognized color word after it renders
/// literally rather than erroring, and its `(...)` argument is ordinary text the
/// outer scan skips. `%d`/`%D` are the ref decorations.
fn check_format(fmt: &str) -> Result<()> {
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            continue;
        }
        match it.next() {
            Some(
                'H' | 'h' | 'T' | 't' | 'P' | 'p' | 's' | 'b' | 'B' | 'f' | 'n' | '%' | 'C' | 'd'
                | 'D' | 'N',
            ) => {}
            Some('a') => match it.next() {
                Some('n' | 'e' | 'd' | 'i' | 'I' | 't' | 'r') => {}
                Some(x) => bail!("unsupported format placeholder %a{x}"),
                None => bail!("unsupported trailing % in format"),
            },
            Some('c') => match it.next() {
                Some('n' | 'e' | 'd' | 'i' | 'I' | 't' | 'r') => {}
                Some(x) => bail!("unsupported format placeholder %c{x}"),
                None => bail!("unsupported trailing % in format"),
            },
            // Signature placeholders: %G? (status char) and %GK (signing key).
            Some('G') => match it.next() {
                Some('?' | 'K') => {}
                Some(x) => bail!("unsupported format placeholder %G{x}"),
                None => bail!("unsupported trailing % in format"),
            },
            // `%xNN` is always accepted: two hex digits emit that byte, and
            // anything else prints literally rather than failing, so there is
            // nothing here to reject.
            Some('x') => {}
            // `%(trailers[:<options>])`, whose option list is validated when it is
            // expanded — an unknown option prints literally there rather than
            // failing here, exactly as git does.
            Some('(') => {}
            Some(x) => bail!("unsupported format placeholder %{x}"),
            None => bail!("unsupported trailing % in format"),
        }
    }
    Ok(())
}

/// Render one commit through a bare user format string, uncolored, undecorated
/// and with the default date mode — git's `pretty_print_commit()` over a
/// `pretty_print_context` that carries nothing but the format.
///
/// This is the entry point `git rebase -i` needs: `sequencer_make_script()`
/// prints each instruction's oneline through `rebase.instructionFormat`, which
/// is an ordinary `--pretty=format:` string.
pub(crate) fn format_commit(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    fmt: &str,
) -> Result<Vec<u8>> {
    let abbrev = std::cell::RefCell::new(AbbrevCache::new(repo));
    let colors = super::color::DecorateColors::disabled();
    let ctx = RenderCtx {
        abbrev_commit: false,
        abbrev: &abbrev,
        date_mode: DateMode::Default,
        extra: Vec::new(),
        want_color: false,
        colors: &colors,
        now: now_secs(),
        decorations: None,
        decorate: DecorateStyle::Off,
        source: None,
        mailmap: None,
        // `rebase -i` renders its instruction lines with no notes; `%N` in an
        // instruction format expands to nothing, as it does under git.
        notes: &[],
        repo,
        mark: "",
        parents: &[],
    };
    let mut out = Vec::new();
    expand_format(&mut out, commit, &unabbreviated(fmt), &ctx)?;
    Ok(out)
}

/// The abbreviating placeholders rewritten to their full-length twins.
///
/// `pretty_print_context pp = {0}` leaves `pp.abbrev` at 0, and
/// `repo_find_unique_abbrev_r()` answers a request for length 0 with the full
/// hash — so `%h`, `%p` and `%t` render exactly what `%H`, `%P` and `%T` do
/// under a zeroed context. `git rebase -i` is one such caller, which is why a
/// `rebase.instructionFormat` of `%h %s` puts a *full* object id in the sheet.
///
/// `%%h` is an escaped percent followed by a literal `h` and is left alone.
fn unabbreviated(fmt: &str) -> String {
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::with_capacity(fmt.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '%' || i + 1 >= chars.len() {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        out.push('%');
        out.push(match chars[i + 1] {
            'h' => 'H',
            'p' => 'P',
            't' => 'T',
            other => other,
        });
        i += 2;
    }
    out
}

/// Expand the placeholders accepted by [`check_format`] for `commit`, using the
/// render knobs in `ctx` (`--date=`, color enablement, decorations, and the clock
/// for relative dates).
/// A `%(trailers...)` placeholder starting at `chars[i] == '('`: its option text
/// and the index just past the closing paren. `None` for anything else that opens
/// with `%(`, which is `%C(...)`'s territory or a malformed placeholder.
fn trailers_placeholder(chars: &[char], i: usize) -> Option<(String, usize)> {
    let close = chars[i..].iter().position(|&c| c == ')')? + i;
    let inner: String = chars[i + 1..close].iter().collect();
    let spec = inner
        .strip_prefix("trailers:")
        .or_else(|| (inner == "trailers").then_some(""))?;
    Some((spec.to_string(), close + 1))
}

fn expand_format(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    fmt: &str,
    ctx: &RenderCtx<'_>,
) -> Result<()> {
    let date_mode = ctx.date_mode;
    // `%C(auto)` latches auto-coloring on for the placeholders that follow it —
    // notably `%d`/`%D`, which stay uncolored until it appears (matching git).
    let mut auto = false;
    // Signature evaluation (gpg/ssh) is lazy and computed at most once per commit,
    // shared between %G? and %GK.
    let mut gsig: Option<(crate::gitsig::GStatus, String)> = None;
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '%' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        let Some(&p) = chars.get(i) else { break };
        // `%(trailers[:<options>])` — the one parenthesised placeholder that is not
        // a colour request. `format_trailers_from_commit()` renders the message's
        // trailer block; an unparsable option list makes git print the placeholder
        // literally rather than fail, which is what a `None` here reproduces.
        if p == '(' {
            if let Some((spec, next)) = trailers_placeholder(&chars, i) {
                match super::interpret_trailers::PrettyOpts::parse(spec.as_bytes()) {
                    Some(opts) => {
                        out.extend_from_slice(&super::interpret_trailers::format_pretty(
                            commit.message_raw()?,
                            &opts,
                        ));
                        i = next;
                        continue;
                    }
                    None => {
                        out.extend_from_slice(b"%(");
                        i += 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
        match p {
            // Under `%C(auto)`, git paints the commit hash with `color.diff.commit`.
            'H' => push_maybe_auto(out, &commit.id().to_string(), auto_commit_color(ctx, auto)),
'h' => push_maybe_auto(
                out,
                &ctx.abbrev.borrow_mut().get(commit.id()),
                auto_commit_color(ctx, auto),
            ),
            'T' => out.extend_from_slice(commit.tree_id()?.to_string().as_bytes()),
't' => {
                out.extend_from_slice(ctx.abbrev.borrow_mut().get(commit.tree_id()?).as_bytes());
            }
            'P' => write_parents(out, commit, false, ctx.abbrev),
            'p' => write_parents(out, commit, true, ctx.abbrev),
            's' => out.extend_from_slice(&subject(commit.message_raw()?)),
            'b' => out.extend_from_slice(&body(commit.message_raw()?)),
            'B' => out.extend_from_slice(commit.message_raw()?),
            // `%N`: the raw note text — no header, no indent — which is the only
            // way a user format shows notes at all.
            'N' => {
                if !ctx.notes.is_empty() {
                    out.extend_from_slice(&super::notes::format_display(
                        ctx.repo,
                        ctx.notes,
                        commit.id().detach(),
                        true,
                    )?);
                }
            }
            'f' => out.extend_from_slice(&sanitized_subject(&subject(commit.message_raw()?))),
            'n' => out.push(b'\n'),
            '%' => out.push(b'%'),
            // `%xNN`: the byte with that hex code, which is how a format asks for
            // a literal tab, NUL or any byte the shell would eat. Two hex digits
            // are required; anything else is not a placeholder at all and git
            // prints the text as typed.
            'x' => match hex_byte(chars.get(i).copied(), chars.get(i + 1).copied()) {
                Some(byte) => {
                    out.push(byte);
                    i += 2;
                }
                None => out.extend_from_slice(b"%x"),
            },
            'C' => expand_color(out, &chars, &mut i, ctx.want_color, &mut auto),
            // `%d`/`%D` are always shown (short by default); `log.decorate=full`
            // / `--decorate=full` switches them to full ref names.
            'd' => expand_decoration(out, commit, ctx, auto, true, ctx.decorate == DecorateStyle::Full),
            'D' => expand_decoration(out, commit, ctx, auto, false, ctx.decorate == DecorateStyle::Full),
            'a' => {
                let author = commit.author()?;
                match chars.get(i).copied() {
                    Some('n') => out.extend_from_slice(author.name),
                    Some('e') => out.extend_from_slice(author.email),
                    Some('d') => expand_date(out, &author, date_mode, ctx.now)?,
                    Some('i') => expand_date(out, &author, DateMode::Iso, ctx.now)?,
                    Some('I') => expand_date(out, &author, DateMode::IsoStrict, ctx.now)?,
                    Some('r') => expand_date(out, &author, DateMode::Relative, ctx.now)?,
                    Some('t') => write!(out, "{}", author.time()?.seconds)?,
                    _ => unreachable!("check_format rejected this already"),
                }
                i += 1;
            }
            'c' => {
                let committer = commit.committer()?;
                match chars.get(i).copied() {
                    Some('n') => out.extend_from_slice(committer.name),
                    Some('e') => out.extend_from_slice(committer.email),
                    Some('d') => expand_date(out, &committer, date_mode, ctx.now)?,
                    Some('i') => expand_date(out, &committer, DateMode::Iso, ctx.now)?,
                    Some('I') => expand_date(out, &committer, DateMode::IsoStrict, ctx.now)?,
                    Some('r') => expand_date(out, &committer, DateMode::Relative, ctx.now)?,
                    Some('t') => write!(out, "{}", committer.time()?.seconds)?,
                    _ => unreachable!("check_format rejected this already"),
                }
                i += 1;
            }
            'G' => {
                let (status, key) =
                    gsig.get_or_insert_with(|| crate::gitsig::evaluate(&commit.data));
                match chars.get(i).copied() {
                    Some('?') => out.push(status.code() as u8),
                    Some('K') => out.extend_from_slice(key.as_bytes()),
                    _ => unreachable!("check_format rejected this already"),
                }
                i += 1;
            }
            _ => unreachable!("check_format rejected this already"),
        }
    }
    Ok(())
}

/// Expand a `%C…` color placeholder starting just past the `C` (index `i` points
/// at the first following char). Advances `i` over whatever the placeholder
/// consumes. Recognizes git's `%Cred`/`%Cgreen`/`%Cblue`/`%Creset` shortcuts and
/// the general `%C(<spec>)` form; anything else leaves `%C` rendered literally.
fn expand_color(out: &mut Vec<u8>, chars: &[char], i: &mut usize, want_color: bool, auto: &mut bool) {
    // git suppresses the `%C(auto)` reset when nothing has been emitted yet for
    // this commit's format, so record that before appending anything.
    let out_empty = out.is_empty();
    let rest: String = chars[*i..].iter().collect();
    // `%C(<spec>)`
    if rest.starts_with('(') {
        if let Some(close) = rest.find(')') {
            let spec = &rest[1..close];
            out.extend_from_slice(parse_color_spec(spec, want_color, auto, out_empty).as_bytes());
            *i += close + 1; // consume through ')'
            return;
        }
        // No closing paren: git prints the rest verbatim. Fall through to literal.
    }
    // Shortcuts.
    for (name, ansi) in [
        ("red", "\x1b[31m"),
        ("green", "\x1b[32m"),
        ("blue", "\x1b[34m"),
        ("reset", "\x1b[m"),
    ] {
        if rest.starts_with(name) {
            if want_color {
                out.extend_from_slice(ansi.as_bytes());
            }
            *i += name.len();
            return;
        }
    }
    // Unrecognized: git renders the `%C` literally and continues.
    out.extend_from_slice(b"%C");
}

/// Parse a `%C(<spec>)` color specification into an ANSI escape (empty when color
/// is disabled). Handles `reset`, `auto`/`auto,<colors>` (which also latches the
/// auto-color flag on), attribute words (`bold`, `dim`, `ul`, …), and up to two
/// color names (foreground then background).
fn parse_color_spec(spec: &str, want_color: bool, auto: &mut bool, out_empty: bool) -> String {
    let spec = spec.trim();
    let colors = if let Some(rest) = spec.strip_prefix("auto") {
        // `%C(auto)` alone enables auto-coloring and emits a reset — but git omits
        // that reset at the very start of a commit's output. `%C(auto,<colors>)`
        // additionally applies those colors.
        *auto = true;
        let rest = rest.strip_prefix(',').unwrap_or(rest).trim();
        if rest.is_empty() {
            return if want_color && !out_empty {
                "\x1b[m".to_string()
            } else {
                String::new()
            };
        }
        rest
    } else {
        spec
    };
    if !want_color {
        return String::new();
    }
    if colors == "reset" {
        return "\x1b[m".to_string();
    }
    let mut codes: Vec<String> = Vec::new();
    let mut foreground = true;
    for tok in colors.split_whitespace() {
        let attr = match tok {
            "bold" => Some("1"),
            "dim" => Some("2"),
            "italic" => Some("3"),
            "ul" | "underline" => Some("4"),
            "blink" => Some("5"),
            "reverse" => Some("7"),
            "strike" => Some("9"),
            "nobold" | "no-bold" => Some("22"),
            _ => None,
        };
        if let Some(a) = attr {
            codes.push(a.to_string());
        } else if let Some(base) = color_base(tok) {
            codes.push((if foreground { base } else { base + 10 }).to_string());
            foreground = false;
        }
    }
    if codes.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// Write a built-in format's commit object name — `<hash>` for `oneline`, the
/// `commit <hash>` header otherwise — in `color.diff.commit`. git's span covers
/// exactly the prefix and the hash: `--parents`, `--source` and the decorations
/// that follow it are all outside, each opening their own color.
fn write_commit_name(out: &mut Vec<u8>, prefix: &[u8], id: &str, ctx: &RenderCtx<'_>) {
    let color = &ctx.colors.commit;
    if !color.is_empty() {
        out.extend_from_slice(color.as_bytes());
    }
    out.extend_from_slice(prefix);
    // `get_revision_mark()`: `--boundary` puts a `-` in front of the object name,
    // after the `commit ` the header formats print.
    out.extend_from_slice(ctx.mark.as_bytes());
    out.extend_from_slice(id.as_bytes());
    if !color.is_empty() {
        out.extend_from_slice(b"\x1b[m");
    }
}

/// The `color.diff.commit` sequence for a `%C(auto)`-gated placeholder: the
/// configured color when this run colors and a `%C(auto)` has been seen, else
/// the empty string (which paints nothing).
fn auto_commit_color<'a>(ctx: &'a RenderCtx<'_>, auto: bool) -> &'a str {
    if auto && ctx.want_color {
        &ctx.colors.commit
    } else {
        ""
    }
}

/// Emit `text` in `commit` — git's `color.diff.commit`, which is the color
/// `%C(auto)` gives the commit hash `%h`/`%H`. An empty `commit` (coloring off, or
/// a spec that selects nothing) emits the text bare, with no reset.
fn push_maybe_auto(out: &mut Vec<u8>, text: &str, commit: &str) {
    if commit.is_empty() {
        out.extend_from_slice(text.as_bytes());
    } else {
        out.extend_from_slice(commit.as_bytes());
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[m");
    }
}

/// Map a color name to its SGR foreground base code (background is `+10`).
fn color_base(name: &str) -> Option<u8> {
    Some(match name {
        "black" => 30,
        "red" => 31,
        "green" => 32,
        "yellow" => 33,
        "blue" => 34,
        "magenta" => 35,
        "cyan" => 36,
        "white" => 37,
        "default" | "normal" => 39,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Decorations (%d / %D)
// ---------------------------------------------------------------------------

/// The kinds of ref a commit can be decorated with, in git's color scheme —
/// `log-tree.c`'s `decoration_colors[]` indexed by `enum decoration_type`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DecoKind {
    /// `HEAD` itself (bold cyan), the entry the `HEAD -> <branch>` fold hangs off.
    Head,
    Tag,
    LocalBranch,
    RemoteBranch,
    /// The single `refs/stash` ref (bold magenta).
    Stash,
    /// Any other ref, reachable only once the default namespace filter is
    /// relaxed by `--clear-decorations` / `log.initialDecorationSet=all`.
    /// git's `DECORATION_NONE`, whose color slot is a bare reset.
    Other,
}

/// One ref pointing at a commit, stored under its full name
/// (`refs/remotes/origin/main`); `--decorate=short` prettifies it at render time.
struct Deco {
    kind: DecoKind,
    full: String,
}

/// The ref→commit map plus HEAD state needed to render `%d`/`%D`.
pub(crate) struct Decorations {
    /// Commit oid → the refs pointing at it (annotated tags peeled through to
    /// their commit), including `HEAD` itself when it survived the filter.
    map: HashMap<ObjectId, Vec<Deco>>,
    /// The full refname HEAD symbolically points at (`refs/heads/main`), for the
    /// `HEAD -> <branch>` fold. `None` when HEAD is detached or unborn.
    head_branch: Option<String>,
}

impl Decorations {
    /// `get_name_decoration()`: whether any ref points at this commit, which is
    /// the whole of `--simplify-by-decoration`s interest in them.
    pub(crate) fn decorates(&self, id: &ObjectId) -> bool {
        self.map.contains_key(id)
    }
}

/// git's `prettify_refname`: strip the three namespaces whose short form is
/// unambiguous. Everything else (`refs/stash`, `refs/custom/thing`) is shown in
/// full even under `--decorate=short`.
fn prettify_refname(full: &str) -> &str {
    full.strip_prefix("refs/heads/")
        .or_else(|| full.strip_prefix("refs/tags/"))
        .or_else(|| full.strip_prefix("refs/remotes/"))
        .unwrap_or(full)
}

/// One normalized decoration-filter pattern — the product of git's
/// `refs.c:normalize_glob_ref`.
struct RefPattern {
    /// The pattern with `refs/` prepended unless it already started with `refs/`
    /// or was the literal `HEAD`, and any trailing `/` stripped.
    text: String,
    /// git's `item->util`: set when the *original* pattern held no glob
    /// metacharacter (`has_glob_specials` = `strpbrk(pattern, "?*[")`), which
    /// turns matching into a `/`-bounded prefix test instead of a wildmatch.
    literal: bool,
}

impl RefPattern {
    /// git's `refs.c:normalize_glob_ref` with a `NULL` prefix.
    fn new(pattern: &str) -> RefPattern {
        let mut text = String::new();
        if !pattern.starts_with("refs/") && pattern != "HEAD" {
            text.push_str("refs/");
        }
        text.push_str(pattern);
        if text.ends_with('/') {
            text.pop();
        }
        RefPattern {
            text,
            literal: !pattern.contains(['?', '*', '[']),
        }
    }

    /// git's `log-tree.c:match_ref_pattern`: a literal pattern matches a whole
    /// path prefix (`refs/heads` matches `refs/heads/main` but not
    /// `refs/headsfoo`), a glob pattern goes through `wildmatch(…, 0)`.
    fn matches(&self, refname: &str) -> bool {
        if self.literal {
            match refname.strip_prefix(&self.text) {
                Some(rest) => rest.is_empty() || rest.starts_with('/'),
                None => false,
            }
        } else {
            wildmatch(self.text.as_bytes(), refname.as_bytes())
        }
    }
}

/// git's `struct decoration_filter`: which refs are allowed to decorate a commit.
///
/// The three lists are consulted in git's order (`log-tree.c:ref_filter_match`):
/// a `--decorate-refs-exclude` hit rejects outright; otherwise, when
/// `--decorate-refs` was given at all, only a hit there is kept; otherwise a
/// `log.excludeDecoration` hit rejects. Anything else is decorated.
pub(crate) struct DecorationFilter {
    include: Vec<RefPattern>,
    exclude: Vec<RefPattern>,
    exclude_config: Vec<RefPattern>,
}

/// The refs git decorates by default — `refs.c:ref_namespace[]` filtered to the
/// entries that carry a `decoration` type, in declaration order. Used verbatim
/// as the default `include` list, which is why an unknown namespace such as
/// `refs/bisect/` is invisible until `--clear-decorations` drops this list.
const DEFAULT_DECORATION_NAMESPACES: [&str; 6] = [
    "HEAD",
    "refs/heads/",
    "refs/tags/",
    "refs/remotes/",
    "refs/stash",
    "refs/replace/",
];

impl DecorationFilter {
    /// git's `builtin/log.c:set_default_decoration_filter` followed by the
    /// normalization `load_ref_decorations` performs on all three lists.
    ///
    /// `use_default` is git's `use_default_decoration_filter`, which starts set
    /// and is cleared by `--clear-decorations`. `log.excludeDecoration` is read
    /// unconditionally — and because a non-empty list of any kind suppresses the
    /// namespace defaults, configuring it alone also exposes refs outside the
    /// known namespaces.
    pub(crate) fn build(
        repo: &gix::Repository,
        include_cli: &[String],
        exclude_cli: &[String],
        mut use_default: bool,
    ) -> DecorationFilter {
        let snap = repo.config_snapshot();
        let mut include: Vec<RefPattern> = include_cli.iter().map(|p| RefPattern::new(p)).collect();
        let exclude: Vec<RefPattern> = exclude_cli.iter().map(|p| RefPattern::new(p)).collect();
        // `log.excludeDecoration` is multi-valued: git appends every occurrence
        // across the whole config hierarchy rather than letting the last win.
        let exclude_config: Vec<RefPattern> = snap
            .plumbing()
            .strings("log.excludeDecoration")
            .into_iter()
            .flatten()
            .map(|v| RefPattern::new(&v.to_str_lossy()))
            .collect();

        // `log.initialDecorationSet=all` relaxes the filter exactly as
        // `--clear-decorations` does.
        if use_default
            && snap
                .string("log.initialDecorationSet")
                .is_some_and(|v| v.to_str_lossy() == "all")
        {
            use_default = false;
        }
        if use_default
            && include.is_empty()
            && exclude.is_empty()
            && exclude_config.is_empty()
        {
            include.extend(DEFAULT_DECORATION_NAMESPACES.iter().map(|n| RefPattern::new(n)));
        }

        DecorationFilter {
            include,
            exclude,
            exclude_config,
        }
    }

    /// Port of `log-tree.c:ref_filter_match`.
    fn matches(&self, refname: &str) -> bool {
        if self.exclude.iter().any(|p| p.matches(refname)) {
            return false;
        }
        if !self.include.is_empty() {
            return self.include.iter().any(|p| p.matches(refname));
        }
        if self.exclude_config.iter().any(|p| p.matches(refname)) {
            return false;
        }
        true
    }
}

/// Does this format use a decoration placeholder, so the ref map is worth
/// building? `%%d` (an escaped percent then a literal `d`) does not count.
fn pretty_uses_decoration(pretty: &Pretty) -> bool {
    let Pretty::User(fmt) = pretty else {
        return false;
    };
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c == '%' && matches!(it.next(), Some('d' | 'D')) {
            return true;
        }
    }
    false
}

/// Build the commit→refs decoration map — git's `load_ref_decorations`: every
/// ref that survives `filter` (peeled through annotated tags to its commit),
/// then `HEAD`, which git adds last and therefore renders first.
///
/// `refs/replace/*` is skipped: git turns those into a `replaced` decoration on
/// the object being *replaced*, which is a mechanism this port does not model,
/// so the ref decorating its own target would be plainly wrong.
pub(crate) fn build_decorations(repo: &gix::Repository, filter: &DecorationFilter) -> Result<Decorations> {
    let mut map: HashMap<ObjectId, Vec<Deco>> = HashMap::new();
    for r in repo.references()?.all()? {
        let r = r.map_err(|e| anyhow!("{e}"))?;
        let Ok(full) = r.name().as_bstr().to_str().map(str::to_owned) else {
            continue;
        };
        if !filter.matches(&full) || full.starts_with("refs/replace/") {
            continue;
        }
        // git's `add_ref_decoration` classifies by the first `ref_namespace[]`
        // entry the refname matches; anything unclaimed is `DECORATION_NONE`.
        let kind = if full.starts_with("refs/heads/") {
            DecoKind::LocalBranch
        } else if full.starts_with("refs/tags/") {
            DecoKind::Tag
        } else if full.starts_with("refs/remotes/") {
            DecoKind::RemoteBranch
        } else if full == "refs/stash" {
            DecoKind::Stash
        } else {
            DecoKind::Other
        };
        // Peel through annotated tags so a tag ref decorates its target commit.
        let Ok(id) = r.into_fully_peeled_id() else {
            continue;
        };
        map.entry(id.detach()).or_default().push(Deco { kind, full });
    }

    let mut head_branch = None;
    if filter.matches("HEAD") {
        if let Ok(head) = repo.head() {
            if let Some(name) = head.referent_name() {
                if let Ok(full) = name.as_bstr().to_str() {
                    if full.starts_with("refs/") {
                        head_branch = Some(full.to_string());
                    }
                }
            }
            if let Some(id) = head.id() {
                map.entry(id.detach()).or_default().push(Deco {
                    kind: DecoKind::Head,
                    full: "HEAD".to_string(),
                });
            }
        }
    }

    Ok(Decorations { map, head_branch })
}

/// Expand `%d` (`wrap` true: ` (…)`) or `%D` (`wrap` false: bare) for `commit`.
/// Colored only when `auto` (set by a preceding `%C(auto)`) and color is enabled,
/// matching git, whose decorations stay plain until `%C(auto)` appears.
fn expand_decoration(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    ctx: &RenderCtx<'_>,
    auto: bool,
    wrap: bool,
    full_refs: bool,
) {
    let Some(decos) = ctx.decorations else {
        return;
    };
    // git's decorations stay plain until a `%C(auto)` turns coloring on for the
    // rest of the format, so an un-auto'd run gets the disabled table.
    let disabled;
    let colors = if auto && ctx.want_color {
        ctx.colors
    } else {
        disabled = super::color::DecorateColors::disabled();
        &disabled
    };
    format_decorations(out, decos, &commit.id().detach(), full_refs, colors, wrap);
}

/// Port of `log-tree.c:format_decorations`: the ` (HEAD -> main, tag: v1)` list
/// for one commit. `wrap` picks `%d`'s parenthesised form over `%D`'s bare one,
/// `full_refs` picks `--decorate=full` over `short`, and `colors` supplies git's
/// `decoration_colors[]` as configured by `color.decorate.<slot>` (the disabled
/// table renders the list uncolored). Emits nothing when the commit carries no
/// surviving decoration.
pub(crate) fn format_decorations(
    out: &mut Vec<u8>,
    decos: &Decorations,
    id: &ObjectId,
    full_refs: bool,
    colors: &super::color::DecorateColors,
    wrap: bool,
) {
    let Some(refs) = decos.map.get(id) else {
        return;
    };
    if refs.is_empty() {
        return;
    }

    const RESET: &str = "\x1b[m";
    let paint = |text: &str, code: &str| -> String {
        if code.is_empty() {
            text.to_string()
        } else {
            format!("{code}{text}{RESET}")
        }
    };
    // git's slot defaults: HEAD bold cyan, local branch bold green, remote bold
    // red, tag bold yellow, stash bold magenta, anything else a bare reset. The
    // punctuation between and around the entries takes `color.diff.commit`, the
    // same color the commit object name it follows is painted with.
    let punct = |text: &str| paint(text, &colors.commit);
    let color_of = |kind: DecoKind| match kind {
        DecoKind::Head => colors.head.as_str(),
        DecoKind::LocalBranch => colors.branch.as_str(),
        DecoKind::RemoteBranch => colors.remote_branch.as_str(),
        DecoKind::Tag => colors.tag.as_str(),
        DecoKind::Stash => colors.stash.as_str(),
        DecoKind::Other => colors.none.as_str(),
    };
    let show = |d: &Deco| -> String {
        // `--decorate=full` / `log.decorate=full` renders the full ref name
        // (`refs/heads/main`) in place of the prettified one (`main`).
        if full_refs {
            d.full.clone()
        } else {
            prettify_refname(&d.full).to_string()
        }
    };

    // git's `current_pointed_by_HEAD`: the `HEAD -> <branch>` fold happens only
    // when BOTH the `HEAD` decoration and the local branch it resolves to are on
    // this commit and survived the filter. The branch is then not listed twice.
    let head_here = refs.iter().any(|d| d.kind == DecoKind::Head);
    let folded: Option<&Deco> = head_here.then(|| decos.head_branch.as_deref()).flatten().and_then(
        |branch| {
            refs.iter()
                .find(|d| d.kind == DecoKind::LocalBranch && d.full == branch)
        },
    );

    // git prepends each decoration as it iterates refs in ascending full-refname
    // order and adds `HEAD` last, so the display order is `HEAD` first and then
    // DESCENDING full refname: refs/heads/dev, refs/heads/feature, refs/tags/v1
    // -> (tag: v1, feature, dev).
    let mut ordered: Vec<&Deco> = refs
        .iter()
        .filter(|d| folded.is_none_or(|f| !std::ptr::eq(*d, f)))
        .collect();
    ordered.sort_by_key(|d| (d.kind != DecoKind::Head, std::cmp::Reverse(d.full.clone())));

    let mut entries: Vec<String> = Vec::new();
    for d in ordered {
        let mut entry = String::new();
        // git colors the `tag: ` prefix and the tag name as two separate
        // bold-yellow spans.
        if d.kind == DecoKind::Tag {
            entry.push_str(&paint("tag: ", color_of(d.kind)));
        }
        entry.push_str(&paint(&show(d), color_of(d.kind)));
        if d.kind == DecoKind::Head {
            if let Some(f) = folded {
                entry.push_str(&punct(" -> "));
                entry.push_str(&paint(&show(f), color_of(f.kind)));
            }
        }
        entries.push(entry);
    }

    // `%d` wraps in ` (…)`; `%D` emits the bare, comma-separated list.
    if wrap {
        out.extend_from_slice(punct(" (").as_bytes());
    }
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(punct(", ").as_bytes());
        }
        out.extend_from_slice(e.as_bytes());
    }
    if wrap {
        out.extend_from_slice(punct(")").as_bytes());
    }
}

/// Current time in epoch seconds, for relative dates. Delegates to the shared
/// resolver so `%cr`/`%ar`/`--date=relative` honor `GIT_TEST_DATE_NOW` like git.
fn now_secs() -> i64 {
    crate::date::now_seconds()
}

/// `-S<string>` over a set of commits, keeping those whose first-parent diff
/// changes the number of occurrences of `needle`.
///
/// This is git's `has_changes` (diffcore-pickaxe.c): for each changed path,
/// count the needle in the whole old blob and the whole new blob, and keep the
/// commit as soon as one pair's counts differ. No patch is built and no line
/// diff is run — the needle's position is irrelevant, only how many times it
/// appears, and a blob whose id is unchanged cannot change its own count.
///
/// The commits are independent, so the scan runs across the thread pool. Each
/// worker owns a repository handle, which is not `Sync`.
fn pickaxe_by_count(repo: &gix::Repository, nodes: Vec<Node>, needle: &[u8]) -> Result<Vec<Node>> {
    if needle.is_empty() || nodes.is_empty() {
        return Ok(nodes);
    }
    // Two commits per worker: a single commit's scan can read many blobs, so
    // there is real work in each unit.
    let workers = crate::threads::count(nodes.len(), 2);
    if workers <= 1 {
        let mut kept = Vec::new();
        for node in nodes {
            if commit_changes_count(repo, &node, needle)? {
                kept.push(node);
            }
        }
        return Ok(kept);
    }

    let cursor = std::sync::atomic::AtomicUsize::new(0);
    let mut hits: Vec<usize> = Vec::new();
    let mut failure: Option<anyhow::Error> = None;
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        let nodes = &nodes;
        for _ in 0..workers {
            let proto = repo.clone();
            let cursor = &cursor;
            handles.push(scope.spawn(move || -> Result<Vec<usize>> {
                let repo = proto;
                let mut mine = Vec::new();
                loop {
                    let i = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(node) = nodes.get(i) else { break };
                    if commit_changes_count(&repo, node, needle)? {
                        mine.push(i);
                    }
                }
                Ok(mine)
            }));
        }
        for h in handles {
            match h.join() {
                Ok(Ok(mine)) => hits.extend(mine),
                Ok(Err(e)) => {
                    failure.get_or_insert(e);
                }
                Err(_) => {
                    failure.get_or_insert_with(|| anyhow::anyhow!("pickaxe worker panicked"));
                }
            }
        }
    });
    if let Some(e) = failure {
        return Err(e);
    }

    hits.sort_unstable();
    let mut keep = vec![false; nodes.len()];
    for i in hits {
        keep[i] = true;
    }
    Ok(nodes.into_iter().zip(keep).filter(|(_, k)| *k).map(|(n, _)| n).collect())
}

/// `true` when this commit's first-parent diff changes how many times `needle`
/// occurs in any one file.
fn commit_changes_count(repo: &gix::Repository, node: &Node, needle: &[u8]) -> Result<bool> {
    let new_tree = repo.find_object(node.id)?.try_into_commit()?.tree()?;
    let old_tree = match node.parents.first() {
        Some(pid) => Some(repo.find_object(*pid)?.try_into_commit()?.tree()?),
        None => None,
    };
    // Counting a blob means reading it, so the count is memoized per blob id
    // within the commit: a file that appears on both sides of several changes
    // (or a tree that reuses a blob) is read once.
    let mut counted: std::collections::HashMap<ObjectId, i64> = std::collections::HashMap::new();
    let mut count_of = |repo: &gix::Repository, id: Option<ObjectId>| -> Result<i64> {
        let Some(id) = id else { return Ok(0) };
        if let Some(n) = counted.get(&id) {
            return Ok(*n);
        }
        // A gitlink or a missing object counts as absent, exactly as git's
        // pickaxe treats a side it cannot read as an empty buffer.
        let n = match repo.find_object(id) {
            Ok(obj) if obj.kind == gix::object::Kind::Blob => {
                count_occurrences(&obj.data, needle)
            }
            _ => 0,
        };
        counted.insert(id, n);
        Ok(n)
    };

    // Two passes, because rename detection is expensive and almost never
    // changes the answer.
    //
    // git runs diffcore's rename pass BEFORE the pickaxe, so content moved from
    // one path to another arrives as a single pair whose two sides hold the
    // needle the same number of times — no change, no match. Pairing can only
    // ever CANCEL a difference, never create one: an unpaired deletion and
    // addition compare against nothing, and joining them can only bring the two
    // counts closer. So a first pass with no rename tracking is a strict
    // over-approximation, and only a commit it flags needs the second, exact
    // pass. Most commits are not flagged, and the history's renames are paid for
    // only where they might matter.
    if !any_count_changed(repo, old_tree.as_ref(), &new_tree, &mut count_of, false)? {
        return Ok(false);
    }
    any_count_changed(repo, old_tree.as_ref(), &new_tree, &mut count_of, true)
}

/// Whether any changed pair between the two trees holds the needle a different
/// number of times, with git's rename tracking (50% similarity, no copies)
/// either on or off.
fn any_count_changed(
    repo: &gix::Repository,
    old_tree: Option<&gix::Tree<'_>>,
    new_tree: &gix::Tree<'_>,
    count_of: &mut impl FnMut(&gix::Repository, Option<ObjectId>) -> Result<i64>,
    rename_tracking: bool,
) -> Result<bool> {
    let mut options = gix::diff::Options::default();
    if rename_tracking {
        options.track_rewrites(Some(Default::default()));
    }
    let changes = repo.diff_tree_to_tree(old_tree, Some(new_tree), Some(options))?;
    for change in changes {
        let (old_id, new_id) = change_blob_ids(&change);
        // An unchanged blob id on both sides cannot change its own count.
        if old_id == new_id {
            continue;
        }
        if count_of(repo, old_id)? != count_of(repo, new_id)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The old and new blob ids of a tree change, or `None` for a side that does not
/// exist (an addition has no old side, a deletion no new one).
fn change_blob_ids(change: &gix::object::tree::diff::ChangeDetached) -> (Option<ObjectId>, Option<ObjectId>) {
    use gix::object::tree::diff::ChangeDetached as C;
    match change {
        C::Addition { id, .. } => (None, Some(*id)),
        C::Deletion { id, .. } => (Some(*id), None),
        C::Modification { previous_id, id, .. } => (Some(*previous_id), Some(*id)),
        C::Rewrite { source_id, id, .. } => (Some(*source_id), Some(*id)),
    }
}

/// Whether a commit's patch satisfies the pickaxe filter, scanning only the
/// added/removed content lines (git's `-S`/`-G` operate on the change text).
///
/// * `-S<string>`: the net occurrence count changed — occurrences on `+` lines
///   minus occurrences on `-` lines is non-zero. This equals git's
///   count-after − count-before, because only changed lines move the total.
/// * `-G<regex>`: some added or removed line matches the regex.
pub(crate) fn pickaxe_hit(
    patch: &[u8],
    needle: Option<&str>,
    re: Option<&regex::bytes::Regex>,
) -> bool {
    let mut net: i64 = 0;
    for line in patch.split(|&b| b == b'\n') {
        // Only real content changes; skip the `+++`/`---` file headers.
        let (sign, content) = match line.first() {
            Some(b'+') if !line.starts_with(b"+++") => (1i64, &line[1..]),
            Some(b'-') if !line.starts_with(b"---") => (-1i64, &line[1..]),
            _ => continue,
        };
        if let Some(re) = re {
            if re.is_match(content) {
                return true;
            }
        }
        if let Some(needle) = needle {
            if !needle.is_empty() {
                net += sign * count_occurrences(content, needle.as_bytes());
            }
        }
    }
    // `-G` reached here without matching (or was absent); `-S` hits on net != 0.
    needle.is_some() && net != 0
}

/// Non-overlapping occurrences of `needle` in `hay`, matching git's pickaxe count.
fn count_occurrences(hay: &[u8], needle: &[u8]) -> i64 {
    if needle.is_empty() || needle.len() > hay.len() {
        return 0;
    }
    // Non-overlapping, like git's kwset walk: a match advances past the whole
    // needle. The substring search is vectorized (git uses the same idea through
    // its kwset); a byte-at-a-time loop reads a multi-megabyte blob far slower.
    memchr::memmem::find_iter(hay, needle).fold((0i64, 0usize), |(n, next), at| {
        if at >= next {
            (n + 1, at + needle.len())
        } else {
            (n, next)
        }
    })
    .0
}

/// git's approxidate for `--since`/`--until`: parse an absolute or relative date
/// to epoch seconds, resolving relative dates against `GIT_TEST_DATE_NOW`/now.
/// An unparseable value falls back to now, matching git's lenient behavior.
pub(crate) fn approxidate(value: &str) -> i64 {
    let now_s = crate::date::now_seconds();
    if value.trim() == "now" {
        return now_s;
    }
    let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(now_s.max(0) as u64);
    gix::date::parse(value, Some(now))
        .map(|t| t.seconds)
        .unwrap_or(now_s)
}

/// git's `show_date_relative`, via the shared port (exact thresholds + the
/// `(diff*24+365)/730` years/months rounding).
fn format_relative(then: i64, now: i64) -> String {
    crate::date::show_date_relative(then, now)
}

/// Write a signature's timestamp in `mode`, the shared body of `%ad`/`%cd` and
/// their fixed-format `%ai`/`%aI` cousins.
fn expand_date(
    out: &mut Vec<u8>,
    sig: &gix::actor::SignatureRef<'_>,
    mode: DateMode,
    now: i64,
) -> Result<()> {
    let t = sig.time()?;
    out.extend_from_slice(fmt_time(t.seconds, t.offset, mode, now).as_bytes());
    Ok(())
}

/// Format a timestamp, routing the clock-relative `relative` mode (which needs
/// `now`) to [`format_relative`] and everything else to [`format_date`].
fn fmt_time(seconds: i64, offset: i32, mode: DateMode, now: i64) -> String {
    match mode {
        DateMode::Relative => format_relative(seconds, now),
        other => format_date(seconds, offset, other),
    }
}

/// git's `%b`: the message body — everything after the blank line that ends the
/// subject paragraph. An empty string when the message is a subject only.
fn body(msg: &[u8]) -> Vec<u8> {
    // Skip leading blank lines, then the subject paragraph, then the single blank
    // line separating it from the body.
    let mut rest = msg;
    while let Some(stripped) = rest.strip_prefix(b"\n") {
        rest = stripped;
    }
    match rest.windows(2).position(|w| w == b"\n\n") {
        Some(pos) => rest[pos + 2..].to_vec(),
        None => Vec::new(),
    }
}

/// git's `%f`: the subject sanitised into a filename — `istitlechar` bytes
/// (alphanumeric, `.`, `_`) kept, every other run folded to a single `-`, runs of
/// `.` collapsed, and trailing `.` trimmed.
fn sanitized_subject(subj: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    // 2 = at start, 1 = a separator run is pending, 0 = mid-word.
    let mut space: u8 = 2;
    let mut i = 0;
    while i < subj.len() {
        let c = subj[i];
        if c.is_ascii_alphanumeric() || c == b'.' || c == b'_' {
            if space == 1 {
                out.push(b'-');
            }
            space = 0;
            out.push(c);
            if c == b'.' {
                while i + 1 < subj.len() && subj[i + 1] == b'.' {
                    i += 1;
                }
            }
        } else {
            space |= 1;
        }
        i += 1;
    }
    while out.last() == Some(&b'.') {
        out.pop();
    }
    out
}

/// Space-separated parent ids, abbreviated for `%p` and full for `%P`.
fn write_parents(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    abbrev: bool,
    cache: &std::cell::RefCell<AbbrevCache>,
) {
    for (i, p) in commit.parent_ids().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        let text = if abbrev {
            cache.borrow_mut().get(p)
        } else {
            p.to_string()
        };
        out.extend_from_slice(text.as_bytes());
    }
}

/// git's subject: the first paragraph of the message, folded onto one line.
fn subject(msg: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for line in msg.split(|&b| b == b'\n') {
        let line = trim_end_ws(line);
        if line.is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        }
        if !out.is_empty() {
            out.push(b' ');
        }
        out.extend_from_slice(line);
    }
    out
}

/// The `pretty_print_commit()` body alone, without the `commit <oid>` line.
///
/// `git log` prints that line from `show_log()` (log-tree.c) and the body from
/// `pretty_print_commit()` (pretty.c); [`render_entry`] fuses the two because
/// every `log` caller wants both. `git rev-list`'s `show_commit()` prints the
/// object name itself — with its own `"commit "` prefix, revision mark and
/// `--parents`/`--children` ids in front — and then calls `pretty_print_commit()`
/// for the rest, so it needs the halves separated the way upstream has them.
///
/// The render knobs are the ones `rev-list` leaves at their defaults: no
/// decoration, no color, no `--date=`, and `revs->abbrev` at `DEFAULT_ABBREV` so
/// `%h` shortens while the object name stays full length.
pub(crate) fn rev_list_pretty_body(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    pretty: &Pretty,
) -> Result<Vec<u8>> {
    let abbrev = std::cell::RefCell::new(AbbrevCache::new(repo));
    let colors = super::color::DecorateColors::disabled();
    let ctx = RenderCtx {
        abbrev_commit: false,
        abbrev: &abbrev,
        date_mode: DateMode::Default,
        extra: Vec::new(),
        want_color: false,
        colors: &colors,
        now: now_secs(),
        decorations: None,
        decorate: DecorateStyle::Off,
        source: None,
        mailmap: None,
        // `cmd_rev_list` never calls `init_display_notes`, so `rev-list --pretty`
        // prints no notes even where `log` would.
        notes: &[],
        repo,
        mark: "",
        parents: &[],
    };
    let mut out = Vec::new();
    match pretty {
        // `pp_title_line()` only: `pretty_print_commit()` skips `pp_remainder()`
        // for oneline, so the body is the subject with no trailing newline.
        Pretty::Oneline => out.extend_from_slice(&subject(commit.message_raw()?)),
        Pretty::User(fmt) => expand_format(&mut out, commit, fmt, &ctx)?,
        Pretty::Reference => {
            let author = commit.author()?;
            let t = author.time()?;
            out.extend_from_slice(abbrev.borrow_mut().get(commit.id()).as_bytes());
            out.extend_from_slice(b" (");
            out.extend_from_slice(&subject(commit.message_raw()?));
            out.extend_from_slice(b", ");
            out.extend_from_slice(
                fmt_time(t.seconds, t.offset, DateMode::Short, ctx.now).as_bytes(),
            );
            out.push(b')');
        }
        Pretty::Raw => {
            // `pp_header()` copies every header line of the object through
            // unchanged under `CMIT_FMT_RAW` — including `gpgsig`, `encoding` and
            // `mergetag`, which a reconstruction from the parsed fields would
            // drop — and stops at the blank line without emitting it.
            let data = commit.data.as_slice();
            let header_len = data
                .windows(2)
                .position(|w| w == b"\n\n")
                .map_or(data.len(), |at| at + 1);
            out.extend_from_slice(&data[..header_len]);
            // `pretty_print_commit()` adds the blank line, then `pp_remainder()`
            // indents the message four spaces with no tab expansion for `raw`.
            out.push(b'\n');
            indent_message(&mut out, commit.message_raw()?, 0);
        }
        Pretty::Medium | Pretty::Short | Pretty::Full | Pretty::Fuller => {
            let author = commit.author()?;
            // `pp_header()` folds the `parent` lines of a merge into one `Merge:`
            // line of abbreviated ids.
            let parents: Vec<_> = commit.parent_ids().collect();
            if parents.len() > 1 {
                out.extend_from_slice(b"Merge:");
                for pid in &parents {
                    out.push(b' ');
                    out.extend_from_slice(abbrev.borrow_mut().get(*pid).as_bytes());
                }
                out.push(b'\n');
            }
            match pretty {
                Pretty::Fuller => {
                    let committer = commit.committer()?;
                    let at = author.time()?;
                    let ct = committer.time()?;
                    write_person(&mut out, b"Author:     ", &author, None);
                    writeln!(
                        out,
                        "AuthorDate: {}",
                        fmt_time(at.seconds, at.offset, ctx.date_mode, ctx.now)
                    )?;
                    write_person(&mut out, b"Commit:     ", &committer, None);
                    writeln!(
                        out,
                        "CommitDate: {}",
                        fmt_time(ct.seconds, ct.offset, ctx.date_mode, ctx.now)
                    )?;
                }
                Pretty::Full => {
                    let committer = commit.committer()?;
                    write_person(&mut out, b"Author: ", &author, None);
                    write_person(&mut out, b"Commit: ", &committer, None);
                }
                _ => {
                    write_person(&mut out, b"Author: ", &author, None);
                    if matches!(pretty, Pretty::Medium) {
                        let t = author.time()?;
                        writeln!(
                            out,
                            "Date:   {}",
                            fmt_time(t.seconds, t.offset, ctx.date_mode, ctx.now)
                        )?;
                    }
                }
            }
            out.push(b'\n');
            if matches!(pretty, Pretty::Short) {
                out.extend_from_slice(b"    ");
                out.extend_from_slice(&subject(commit.message_raw()?));
                out.push(b'\n');
            } else {
                indent_message(&mut out, commit.message_raw()?, 8);
            }
        }
    }
    abbrev.into_inner().flush();
    Ok(out)
}

/// The per-commit rendering knobs threaded down from [`log`].
struct RenderCtx<'a> {
    /// `--abbrev-commit`: shorten the commit id on the header/oneline.
    abbrev_commit: bool,
    /// Memoised abbreviations (see [`AbbrevCache`]); shared, so a `&RenderCtx`
    /// can still record what it computed.
    abbrev: &'a std::cell::RefCell<AbbrevCache>,
    /// `--date=`: the format `%ad`/`%cd` and the `Date`/`*Date` lines follow.
    date_mode: DateMode,
    /// `--parents`: the commit's own parent ids, decorating the header/oneline.
    /// Empty when the flag is off. Full-length ids unless `abbrev_commit`.
    extra: Vec<u8>,
    /// Whether `%C`/`%C(...)` color placeholders and `%C(auto)`-gated decoration
    /// emit ANSI escapes (git's `want_color`).
    want_color: bool,
    /// The `color.decorate.*` slots and `color.diff.commit`, resolved from config;
    /// the disabled table when coloring is off.
    colors: &'a super::color::DecorateColors,
    /// Current time in epoch seconds, for relative dates (`%cr`/`%ar`).
    now: i64,
    /// Commit→refs map plus HEAD info for `%d`/`%D`; `None` when the format has no
    /// decoration placeholder.
    decorations: Option<&'a Decorations>,
    /// `--decorate` / `log.decorate`: the decoration style for the oneline/header
    /// formats. `Off` appends nothing; `Short`/`Full` append ` (refs)` with short
    /// or full ref names. Also selects short-vs-full for the `%d`/`%D`
    /// placeholders (which are shown regardless of `Off`, in short form).
    decorate: DecorateStyle,
    /// `--source`: the ref/argument this commit was reached from, rendered as
    /// `\t<source>` after the hash on the built-in header formats. `None` when
    /// `--source` is off (and for user/`reference` formats, which git leaves bare).
    source: Option<&'a [u8]>,
    /// `--use-mailmap` / `log.mailmap`: rewrites the `Author:`/`Commit:` lines of
    /// the built-in header formats through `.mailmap`. `None` leaves the
    /// identities as the commit recorded them. git applies it in `pp_user_info`
    /// only, so `oneline`, `raw` and user formats are unaffected — `%aN`/`%aE`
    /// consult the mailmap on their own, independent of this flag.
    mailmap: Option<&'a Mailmap>,
    /// The notes trees whose `Notes[ (<ref>)]:` blocks follow the message. Empty
    /// when notes are off; a user format reaches them only through `%N`.
    notes: &'a [super::notes::Tree],
    /// Rendering a note means reading its blob.
    repo: &'a gix::Repository,
    /// `get_revision_mark()`: `- ` for a `--boundary` commit, empty otherwise.
    mark: &'static str,
    /// The commit's effective parents — its own, or the rewritten list a history
    /// simplification left behind. What `Merge:` and `--parents` print.
    parents: &'a [ObjectId],
}

/// The `Notes[ (<ref>)]:` blocks for `commit`, or empty.
///
/// git appends these to the message buffer, so the leading newline
/// `format_display_notes()` emits lands differently per format: after a
/// `medium` message (which already ends in a newline) it renders as the blank
/// line above the block, and after a `oneline` subject it just ends that line.
fn notes_block(commit: &gix::Commit<'_>, ctx: &RenderCtx<'_>) -> Result<Vec<u8>> {
    if ctx.notes.is_empty() {
        return Ok(Vec::new());
    }
    super::notes::format_display(ctx.repo, ctx.notes, commit.id().detach(), false)
}

/// Render one commit's header in the selected format. Built-in formats end with
/// a newline; user formats, `oneline`, and `reference` do not, because their
/// record ending is supplied by the separator/terminator rule in [`log`].
fn render_entry(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    pretty: &Pretty,
    ctx: &RenderCtx<'_>,
) -> Result<()> {
    let id = if ctx.abbrev_commit {
        ctx.abbrev.borrow_mut().get(commit.id())
    } else {
        commit.id().to_string()
    };

    match pretty {
        Pretty::Oneline => {
            write_commit_name(out, b"", &id, ctx);
            out.extend_from_slice(&ctx.extra);
            write_source(out, ctx);
            // `--decorate`: ` (HEAD -> main, tag: v1)` between the hash and subject.
            if ctx.decorate != DecorateStyle::Off {
                expand_decoration(
                    out,
                    commit,
                    ctx,
                    ctx.want_color,
                    true,
                    ctx.decorate == DecorateStyle::Full,
                );
            }
            out.push(b' ');
            out.extend_from_slice(&subject(commit.message_raw()?));
            out.extend_from_slice(&notes_block(commit, ctx)?);
        }
        Pretty::Reference => {
            // `%h (%s, %ad)` with `--date=short` unless `--date=` overrode it.
            let date_mode = match ctx.date_mode {
                DateMode::Default => DateMode::Short,
                other => other,
            };
            let author = commit.author()?;
            let t = author.time()?;
            out.extend_from_slice(ctx.abbrev.borrow_mut().get(commit.id()).as_bytes());
            out.extend_from_slice(b" (");
            out.extend_from_slice(&subject(commit.message_raw()?));
            out.extend_from_slice(b", ");
            out.extend_from_slice(fmt_time(t.seconds, t.offset, date_mode, ctx.now).as_bytes());
            out.push(b')');
        }
        Pretty::User(fmt) => expand_format(out, commit, fmt, ctx)?,
        Pretty::Raw => {
            let author = commit.author()?;
            let committer = commit.committer()?;
            // Raw always shows the full commit id; `--parents` still decorates it.
            write_commit_name(out, b"commit ", &commit.id().to_string(), ctx);
            out.extend_from_slice(&ctx.extra);
            write_source(out, ctx);
            out.push(b'\n');
            writeln!(out, "tree {}", commit.tree_id()?)?;
            for pid in commit.parent_ids() {
                writeln!(out, "parent {pid}")?;
            }
            write_raw_ident(out, b"author", &author)?;
            write_raw_ident(out, b"committer", &committer)?;
            out.push(b'\n');
            // `raw` prints the message as stored: its table entry has no tab width.
            indent_message(out, commit.message_raw()?, 0);
            out.extend_from_slice(&notes_block(commit, ctx)?);
        }
        Pretty::Medium | Pretty::Short | Pretty::Full | Pretty::Fuller => {
            let author = commit.author()?;
            write_commit_name(out, b"commit ", &id, ctx);
            out.extend_from_slice(&ctx.extra);
            write_source(out, ctx);
            // `--decorate`: ` (HEAD -> main, tag: v1)` after the commit id.
            if ctx.decorate != DecorateStyle::Off {
                expand_decoration(
                    out,
                    commit,
                    ctx,
                    ctx.want_color,
                    true,
                    ctx.decorate == DecorateStyle::Full,
                );
            }
            out.push(b'\n');

            // A merge commit lists its abbreviated parents right after `commit`.
            // The list is the *effective* one: history simplification rewrites
            // parents before anything is printed, so a merge whose sides
            // collapsed onto one line is no longer shown as a merge.
            if ctx.parents.len() > 1 {
                out.extend_from_slice(b"Merge:");
                for pid in ctx.parents {
                    out.push(b' ');
                    out.extend_from_slice(ctx.abbrev.borrow_mut().get(pid.attach(ctx.repo)).as_bytes());
                }
                out.push(b'\n');
            }

            match pretty {
                Pretty::Fuller => {
                    let committer = commit.committer()?;
                    let at = author.time()?;
                    let ct = committer.time()?;
                    write_person(out, b"Author:     ", &author, ctx.mailmap);
                    writeln!(
                        out,
                        "AuthorDate: {}",
                        fmt_time(at.seconds, at.offset, ctx.date_mode, ctx.now)
                    )?;
                    write_person(out, b"Commit:     ", &committer, ctx.mailmap);
                    writeln!(
                        out,
                        "CommitDate: {}",
                        fmt_time(ct.seconds, ct.offset, ctx.date_mode, ctx.now)
                    )?;
                }
                Pretty::Full => {
                    let committer = commit.committer()?;
                    write_person(out, b"Author: ", &author, ctx.mailmap);
                    write_person(out, b"Commit: ", &committer, ctx.mailmap);
                }
                _ => {
                    // medium / short
                    write_person(out, b"Author: ", &author, ctx.mailmap);
                    if matches!(pretty, Pretty::Medium) {
                        let time = author.time()?;
                        writeln!(
                            out,
                            "Date:   {}",
                            fmt_time(time.seconds, time.offset, ctx.date_mode, ctx.now)
                        )?;
                    }
                }
            }
            out.push(b'\n');

            if matches!(pretty, Pretty::Short) {
                // `short` shows only the subject, indented four spaces.
                out.extend_from_slice(b"    ");
                out.extend_from_slice(&subject(commit.message_raw()?));
                out.push(b'\n');
            } else {
                indent_message(out, commit.message_raw()?, 8);
            }
            out.extend_from_slice(&notes_block(commit, ctx)?);
        }
    }
    Ok(())
}

/// `--source`: git's `show_log` prints `\t<source>` right after the commit hash
/// (and any `--parents` ids) on the built-in header formats. A no-op when `--source`
/// is off. User and `reference` formats never call this, matching git.
fn write_source(out: &mut Vec<u8>, ctx: &RenderCtx<'_>) {
    if let Some(src) = ctx.source {
        out.push(b'\t');
        out.extend_from_slice(src);
    }
}

/// Write git's `<label> <name> <<email>>` header line, mapped through the
/// mailmap when `--use-mailmap` / `log.mailmap` supplied one — git's
/// `pp_user_info`, which is the single place the built-in formats resolve an
/// identity.
pub(crate) fn write_person(
    out: &mut Vec<u8>,
    label: &[u8],
    sig: &gix::actor::SignatureRef<'_>,
    mailmap: Option<&Mailmap>,
) {
    let (mut name, mut email): (&[u8], &[u8]) = (sig.name, sig.email);
    if let Some(info) = mailmap.and_then(|m| m.lookup(name, email)) {
        if let Some(e) = &info.email {
            email = e;
        }
        if let Some(n) = &info.name {
            name = n;
        }
    }
    out.extend_from_slice(label);
    out.extend_from_slice(name);
    out.extend_from_slice(b" <");
    out.extend_from_slice(email);
    out.extend_from_slice(b">\n");
}

/// git's mailmap lookup structure (`mailmap.c`), built from the entries
/// gitoxide parsed out of the repository's mailmap sources.
///
/// `gix_mailmap::Snapshot::resolve` cannot be used directly: it also normalizes
/// the *case* of the address to the mailmap's spelling, even for an entry that
/// only renames the author. git leaves the address exactly as the commit
/// recorded it there, so `Renamed Nick <NICK@X.com>` keeps its capitals. Only
/// the lookup is reimplemented here; finding, reading and parsing the mailmap
/// files is still gitoxide's (`Repository::open_mailmap`).
#[derive(Default)]
pub(crate) struct Mailmap {
    /// Keyed by the ASCII-lowercased old email, which is how git's `strcasecmp`
    /// comparison behaves.
    by_email: HashMap<Vec<u8>, MailmapEmail>,
}

/// All entries sharing one commit email — git's `struct mailmap_entry`.
#[derive(Default)]
struct MailmapEmail {
    /// The mapping used when no `<old-name>` qualifier matched.
    simple: MailmapInfo,
    /// Name-qualified mappings, keyed by the ASCII-lowercased old name.
    by_name: HashMap<Vec<u8>, MailmapInfo>,
}

/// The replacement name and/or email a matched entry supplies — git's
/// `struct mailmap_info`. An entry with neither is "no match".
#[derive(Default)]
pub(crate) struct MailmapInfo {
    name: Option<Vec<u8>>,
    email: Option<Vec<u8>>,
}

impl Mailmap {
    /// Load every mailmap source gitoxide knows about (worktree `.mailmap`, then
    /// `mailmap.blob`, then `mailmap.file`) and index it git's way.
    pub(crate) fn load(repo: &gix::Repository) -> Mailmap {
        let snapshot = repo.open_mailmap();
        let mut map = Mailmap::default();
        // git's `add_mapping`: a name-qualified line owns its own sub-entry, an
        // unqualified line overrides only the fields it carries.
        for entry in snapshot.entries() {
            let slot = map.by_email.entry(lower_ascii(entry.old_email())).or_default();
            match entry.old_name() {
                None => {
                    if let Some(n) = entry.new_name() {
                        slot.simple.name = Some(n.to_vec());
                    }
                    if let Some(e) = entry.new_email() {
                        slot.simple.email = Some(e.to_vec());
                    }
                }
                Some(old_name) => {
                    slot.by_name.insert(
                        lower_ascii(old_name),
                        MailmapInfo {
                            name: entry.new_name().map(|n| n.to_vec()),
                            email: entry.new_email().map(|e| e.to_vec()),
                        },
                    );
                }
            }
        }
        map
    }

    /// git's `map_user`: find the email, then prefer a name-qualified sub-entry
    /// when one matches, else fall back to the unqualified mapping.
    fn lookup(&self, name: &[u8], email: &[u8]) -> Option<&MailmapInfo> {
        let slot = self.by_email.get(&lower_ascii(email))?;
        let info = if slot.by_name.is_empty() {
            &slot.simple
        } else {
            slot.by_name.get(&lower_ascii(name)).unwrap_or(&slot.simple)
        };
        (info.name.is_some() || info.email.is_some()).then_some(info)
    }
}

/// The ASCII-lowercased lookup key for a mailmap email or name, matching the
/// `strcasecmp` git compares them with.
fn lower_ascii(s: &[u8]) -> Vec<u8> {
    s.iter().map(u8::to_ascii_lowercase).collect()
}

/// Write a raw-format identity line: `<role> <name> <<email>> <seconds> +ZZZZ`.
fn write_raw_ident(out: &mut Vec<u8>, role: &[u8], sig: &gix::actor::SignatureRef<'_>) -> Result<()> {
    let t = sig.time()?;
    let (sign, off) = if t.offset < 0 { ('-', -t.offset) } else { ('+', t.offset) };
    out.extend_from_slice(role);
    out.push(b' ');
    out.extend_from_slice(sig.name);
    out.extend_from_slice(b" <");
    out.extend_from_slice(sig.email);
    out.push(b'>');
    writeln!(
        out,
        " {} {sign}{:02}{:02}",
        t.seconds,
        off / 3600,
        (off % 3600) / 60
    )?;
    Ok(())
}

/// Indent a commit message four spaces per line, exactly as git's `pp_remainder`:
/// every line — blank ones included — is prefixed, and trailing blank lines are
/// dropped.
///
/// `tab_width` is git's `expand_tabs_in_log`, which its format table sets to 8
/// for the formats that indent (`medium`, `full`, `fuller`) and 0 for `raw`. A
/// tab inside a commit message was written against the message's own left edge,
/// so a four-space indent would shift every tab stop and misalign whatever the
/// author lined up; git expands the tabs instead, and the columns survive.
fn indent_message(out: &mut Vec<u8>, msg: &[u8], tab_width: usize) {
    let mut lines: Vec<&[u8]> = msg.split(|&b| b == b'\n').collect();
    while lines.last() == Some(&&b""[..]) {
        lines.pop();
    }
    for line in lines {
        out.extend_from_slice(b"    ");
        if tab_width == 0 {
            out.extend_from_slice(line);
        } else {
            expand_tabs(out, line, tab_width);
        }
        out.push(b'\n');
    }
}

/// git's `strbuf_add_tabexpand`: replace each tab with spaces up to the next tab
/// stop, measuring columns from the START OF THE LINE — the indent the caller
/// already wrote does not count, which is what keeps a message's internal
/// alignment intact.
///
/// Width is display width, so a wide character occupies two columns. A segment
/// that is not valid UTF-8 cannot be measured, and git stops expanding that line
/// and copies the rest verbatim rather than guessing.
fn expand_tabs(out: &mut Vec<u8>, line: &[u8], tab_width: usize) {
    let mut rest = line;
    let mut column = 0usize;
    while let Some(at) = memchr::memchr(b'\t', rest) {
        let Ok(text) = std::str::from_utf8(&rest[..at]) else {
            break;
        };
        column += unicode_width::UnicodeWidthStr::width(text);
        out.extend_from_slice(&rest[..at]);
        out.extend(std::iter::repeat_n(b' ', tab_width - (column % tab_width)));
        column += tab_width - (column % tab_width);
        rest = &rest[at + 1..];
    }
    out.extend_from_slice(rest);
}

// ---------------------------------------------------------------------------
// Per-commit diff
// ---------------------------------------------------------------------------

/// One changed path, with the line counts `--stat` needs.
struct FileChange {
    path: Vec<u8>,
    status: u8,
    added: usize,
    deleted: usize,
    is_binary: bool,
    old_size: usize,
    new_size: usize,
}

/// Diff `commit`'s tree against `parent`'s (or the empty tree for a root commit),
/// dropping the directory entries gix reports alongside the files it recurses into.
/// Blob contents are only read when `with_counts` is set, which is the only case
/// that needs them.
/// Fill the ledger's log caches for the newest `limit` commits reachable from
/// `HEAD`, and report how many commits were covered.
///
/// This is what the daemon calls after a watched repo's refs move. Everything it
/// computes is a pure function of immutable objects — an abbreviation is fixed
/// once the object exists, and a tree pair's change list and line tallies never
/// expire — so the work is valid forever and can be done before anyone asks for
/// it. That is the part git has no way to do: it has no process alive between
/// commands, so the first `log --stat` after a pull always pays full price.
///
/// Bounded by `limit` because only the recent end of a history is ever read
/// interactively, and each pass is a fresh walk from the new tip. Failures are
/// silent by design: a warmed cache is an optimization, and the verb that missed
/// it simply computes the value itself.
pub fn warm_caches(repo: &gix::Repository, limit: usize) -> usize {
    let Ok(head) = repo.head_commit() else { return 0 };
    let mut abbrev = AbbrevCache::new(repo);
    let mut warmed = 0usize;
    let Ok(walk) = repo.rev_walk([head.id]).all() else { return 0 };
    for info in walk.take(limit).flatten() {
        let Ok(commit) = repo.find_commit(info.id) else { continue };
        abbrev.get(commit.id());
        let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
        for parent in &parents {
            abbrev.get(parent.attach(repo));
        }
        // git shows no diff for a merge, so nothing would ever read its tallies.
        if parents.len() < 2 {
            // Stores into the tree-diff cache as a side effect (see
            // `collect_changes`), with the counts every `--stat`-style format
            // needs — the expensive half, one blob read per changed file.
            let _ = collect_changes(repo, &commit, parents.first().copied(), true);
        }
        warmed += 1;
    }
    abbrev.flush();
    warmed
}

fn collect_changes(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    parent: Option<ObjectId>,
    with_counts: bool,
) -> Result<Vec<FileChange>> {
    let new_tree = commit.tree()?;
    let old_tree = match parent {
        Some(pid) => Some(repo.find_object(pid)?.try_into_commit()?.tree()?),
        None => None,
    };

    // A tree-to-tree diff is a pure function of two immutable trees, so the file
    // list — and the per-file line tallies, which cost a blob read each — are
    // memoised exactly as blame is. `--stat` over a range re-diffs the same
    // parent/child pairs on every invocation; git does too, but this sidesteps
    // the work instead of racing it.
    let old_key = old_tree.as_ref().map(|t| t.id.to_string()).unwrap_or_default();
    let new_key = new_tree.id.to_string();
    if let Some(text) = crate::rcache::treediff_load(&old_key, &new_key, with_counts) {
        if let Some(files) = decode_changes(text) {
            return Ok(files);
        }
    }

    let mut changes = repo.diff_tree_to_tree(
        old_tree.as_ref(),
        Some(&new_tree),
        gix::diff::Options::default(),
    )?;
    changes.sort_by(|a, b| change_path(a).cmp(change_path(b)));

    let mut out = Vec::with_capacity(changes.len());
    for change in &changes {
        if let Some(f) = prepare_change(repo, change, with_counts)? {
            out.push(f);
        }
    }
    // Off-thread: the answer is already in `out`, so the row is bookkeeping and
    // the caller must not wait for a transaction to reach the disk.
    crate::rcache::cache_write(crate::rcache::CacheWrite::TreeDiff {
        old_tree: old_key,
        new_tree: new_key,
        counts: with_counts,
        files: encode_changes(&out),
    });
    Ok(out)
}

/// Encode a change list for the ledger: one record per file,
/// `status,added,deleted,binary,old_size,new_size,path`, NUL-separated so a path
/// containing any printable byte survives the round trip.
fn encode_changes(files: &[FileChange]) -> String {
    files
        .iter()
        .map(|f| {
            format!(
                "{},{},{},{},{},{},{}",
                f.status as char,
                f.added,
                f.deleted,
                u8::from(f.is_binary),
                f.old_size,
                f.new_size,
                String::from_utf8_lossy(&f.path)
            )
        })
        .collect::<Vec<_>>()
        .join("\0")
}

/// Decode what [`encode_changes`] wrote. `None` for a malformed record, so a
/// damaged row falls back to a real diff rather than a wrong answer.
fn decode_changes(text: &str) -> Option<Vec<FileChange>> {
    if text.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for rec in text.split('\0') {
        let mut f = rec.splitn(7, ',');
        let status = f.next()?.bytes().next()?;
        let added: usize = f.next()?.parse().ok()?;
        let deleted: usize = f.next()?.parse().ok()?;
        let is_binary = f.next()? == "1";
        let old_size: usize = f.next()?.parse().ok()?;
        let new_size: usize = f.next()?.parse().ok()?;
        let path = f.next()?.as_bytes().to_vec();
        out.push(FileChange { path, status, added, deleted, is_binary, old_size, new_size });
    }
    Some(out)
}

/// Whether the diff between `commit` and `parent` (the empty tree when `None`)
/// touches any of the pathspecs — git's TREESAME test, negated.
/// The name the followed path arrived from, when this commit renamed it —
/// `try_to_follow_renames()` reduced to what `--follow` needs: the path must be new
/// in this commit, and the source is the deletion whose content is most similar.
///
/// git runs its full `diffcore_rename` here (exact matches first, then the 50%
/// similarity pass). The exact pass is reproduced faithfully; the inexact one uses
/// the same `diffcore_count_changes()` estimator, so it agrees on the score but
/// picks its own winner when several deletions score alike.
fn follow_source(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    parent: ObjectId,
    path: &gix::bstr::BString,
) -> Result<Option<gix::bstr::BString>> {
    let files = collect_changes(repo, commit, Some(parent), false)?;
    // The followed path has to be an addition here; anything else is not a rename.
    if !files.iter().any(|f| f.path.as_slice() == path.as_slice() && f.status == b'A') {
        return Ok(None);
    }
    let new_tree = commit.tree()?;
    let old_tree = repo.find_commit(parent)?.tree()?;
    let blob = |tree: &gix::Tree<'_>, p: &gix::bstr::BString| -> Result<Option<(ObjectId, Vec<u8>)>> {
        let Some(entry) = tree.lookup_entry_by_path(gix::path::from_bstr(p.as_bstr()))? else {
            return Ok(None);
        };
        let id = entry.object_id();
        Ok(Some((id, repo.find_object(id)?.detach().data)))
    };
    let Some((new_id, new_bytes)) = blob(&new_tree, path)? else {
        return Ok(None);
    };

    let mut best: Option<(f64, gix::bstr::BString)> = None;
    for f in &files {
        if f.status != b'D' {
            continue;
        }
        let old_name = gix::bstr::BString::from(f.path.clone());
        let Some((old_id, old_bytes)) = blob(&old_tree, &old_name)? else {
            continue;
        };
        // `find_exact_renames()`: an identical blob is a rename outright.
        if old_id == new_id {
            return Ok(Some(old_name));
        }
        let score = similarity_score(&old_bytes, &new_bytes);
        // `DEFAULT_RENAME_SCORE`: half the content has to survive.
        if score >= super::diffcore_rename::MAX_SCORE / 2.0
            && best.as_ref().is_none_or(|(b, _)| score > *b)
        {
            best = Some((score, old_name));
        }
    }
    Ok(best.map(|(_, p)| p))
}

/// `estimate_similarity()` (diffcore-rename.c): how much of `old` survives in
/// `new`, in `MAX_SCORE` units, off the same chunk-hash counter rename detection
/// uses everywhere else.
fn similarity_score(old: &[u8], new: &[u8]) -> f64 {
    if old.is_empty() && new.is_empty() {
        return super::diffcore_rename::MAX_SCORE;
    }
    let max = old.len().max(new.len()) as f64;
    if max == 0.0 {
        return 0.0;
    }
    let (copied, _added) = super::diff_files::count_changes_sides(old, true, new, true);
    (copied as f64 * super::diffcore_rename::MAX_SCORE) / max
}

fn changes_match(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    parent: Option<ObjectId>,
    pathspecs: &[String],
) -> Result<bool> {
    let files = collect_changes(repo, commit, parent, false)?;
    for f in &files {
        for spec in pathspecs {
            if pathspec_matches(spec, &f.path)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Does a plain git pathspec match a repo-relative path? Matches git's default
/// (non-magic) rules: an exact path, a leading directory (`src` matches
/// `src/lib.rs`), or a wildcard matched by git's `wildmatch.c` (flags=0) — `*`/`?`
/// span the whole path and `[…]` bracket expressions (ranges, `!`/`^` negation,
/// POSIX `[:class:]`) are honored. Magic pathspecs (`:(glob)`, `:!exclude`, …)
/// are surfaced terse rather than matched wrong.
pub(crate) fn pathspec_matches(spec: &str, path: &[u8]) -> Result<bool> {
    if spec.starts_with(':') {
        bail!("magic pathspecs are not ported");
    }
    let spec = spec.strip_prefix("./").unwrap_or(spec);
    let spec = spec.trim_end_matches('/');
    if spec.is_empty() || spec == "." {
        return Ok(true);
    }
    let sb = spec.as_bytes();
    if path == sb {
        return Ok(true);
    }
    // Leading-directory match: the path lives under the pathspec directory.
    if path.len() > sb.len() && path.starts_with(sb) && path[sb.len()] == b'/' {
        return Ok(true);
    }
    // A `*`, `?`, or `[` makes this a wildcard pathspec (git's `is_glob_special`
    // set), matched by `wildmatch` below. `[` covers bracket expressions and POSIX
    // classes.
    if spec.bytes().any(|b| b == b'*' || b == b'?' || b == b'[') {
        return Ok(wildmatch(sb, path));
    }
    Ok(false)
}

/// Glob match for a plain (non-magic) pathspec, delegating to the faithful
/// `wildmatch.c:dowild` port below. Only git's `WM_MATCH` counts as a match, so a
/// malformed pattern (`WM_ABORT_ALL`) is reported as no-match, exactly as git's
/// pathspec callers treat `wildmatch(...) != 0`.
pub(crate) fn wildmatch(pat: &[u8], text: &[u8]) -> bool {
    matches!(dowild(pat, text), Wm::Match)
}

/// Return states of git's `wildmatch.c:dowild`, specialised to the `flags == 0`
/// case a non-magic ("plain") git pathspec uses: `dir.c:git_fnmatch` calls
/// `wildmatch(pattern, string, 0)` for a pathspec without `:(glob)`/`:(icase)`
/// magic (dir.c: "wildmatch has not learned no FNM_PATHNAME mode yet"). With
/// `flags == 0` there is no `WM_PATHNAME` (so `*`/`?`/`[…]` all span `/`) and no
/// `WM_CASEFOLD`; that also means the `WM_ABORT_TO_STARSTAR` state cannot arise
/// (it needs `match_slash == 0`, but here `*` behaves as `**`), so only these
/// three outcomes remain.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wm {
    /// `WM_MATCH`.
    Match,
    /// `WM_NOMATCH`.
    NoMatch,
    /// `WM_ABORT_ALL`: text ended with the pattern still expecting a literal, or a
    /// malformed bracket expression. A no-match at the top level.
    AbortAll,
}

/// git's `is_glob_special`: the bytes `wildmatch` treats as metacharacters.
fn is_glob_special(c: u8) -> bool {
    matches!(c, b'*' | b'?' | b'[' | b'\\')
}

/// `pat.get(i)` as a byte, using `0` (git's NUL terminator) past the end.
fn at(pat: &[u8], i: usize) -> u8 {
    pat.get(i).copied().unwrap_or(0)
}

/// Faithful port of `wildmatch.c:dowild` for `flags == 0` (see [`Wm`]). Matches
/// pattern `pat` against `text`.
fn dowild(pat: &[u8], text: &[u8]) -> Wm {
    let mut p = 0usize;
    let mut t = 0usize;
    while p < pat.len() {
        let mut p_ch = pat[p];
        let t_ch = if t < text.len() { text[t] } else { 0 };
        // `if ((t_ch = *text) == '\0' && p_ch != '*') return WM_ABORT_ALL;`
        if t_ch == 0 && p_ch != b'*' {
            return Wm::AbortAll;
        }
        match p_ch {
            // `case '?'`: flags=0 matches any char, `/` included.
            b'?' => {}
            b'*' => {
                // Collapse a run of `*`; with flags=0, `*` behaves as `**`
                // (`match_slash` is always true).
                p += 1;
                while p < pat.len() && pat[p] == b'*' {
                    p += 1;
                }
                // Trailing `*`/`**` matches the remaining text unconditionally.
                if p >= pat.len() {
                    return Wm::Match;
                }
                loop {
                    if t >= text.len() {
                        break;
                    }
                    // When the char after `*` is a literal, fast-forward the text to
                    // it: everything skipped must belong to the `*`.
                    if !is_glob_special(pat[p]) {
                        let lit = pat[p];
                        while t < text.len() && text[t] != lit {
                            t += 1;
                        }
                        if t >= text.len() {
                            return Wm::NoMatch;
                        }
                    }
                    match dowild(&pat[p..], &text[t..]) {
                        Wm::NoMatch => {}
                        other => return other,
                    }
                    t += 1;
                }
                return Wm::AbortAll;
            }
            b'[' => match bracket(pat, &mut p, t_ch) {
                // On a match `p` is left on the `]`; the advance below steps past it.
                Wm::Match => {}
                nonmatch => return nonmatch,
            },
            b'\\' => {
                // Literal match with the following char. `p[1] == '\0'` falls out as
                // `p_ch == 0`, which the `t_ch != p_ch` test rejects (t_ch != 0
                // here), exactly as git's `default` arm handles it.
                p += 1;
                p_ch = at(pat, p);
                if t_ch != p_ch {
                    return Wm::NoMatch;
                }
            }
            _ => {
                if t_ch != p_ch {
                    return Wm::NoMatch;
                }
            }
        }
        p += 1;
        t += 1;
    }
    if t < text.len() {
        Wm::NoMatch
    } else {
        Wm::Match
    }
}

/// Port of the `case '['` block of `wildmatch.c:dowild` (flags=0). `*p` enters on
/// the `[` and, on a match/no-match decision, is left on the closing `]` so the
/// caller's single advance steps past it. Returns [`Wm::AbortAll`] for a malformed
/// class (missing `]`), matching git.
fn bracket(pat: &[u8], p: &mut usize, t_ch: u8) -> Wm {
    // `p_ch = *++p`
    *p += 1;
    let mut p_ch = at(pat, *p);
    // NEGATE_CLASS2 `^` is normalised to NEGATE_CLASS `!`.
    if p_ch == b'^' {
        p_ch = b'!';
    }
    let negated = p_ch == b'!';
    if negated {
        *p += 1;
        p_ch = at(pat, *p);
    }
    let mut prev_ch: u8 = 0;
    let mut matched = false;
    loop {
        if p_ch == 0 {
            return Wm::AbortAll;
        }
        if p_ch == b'\\' {
            *p += 1;
            p_ch = at(pat, *p);
            if p_ch == 0 {
                return Wm::AbortAll;
            }
            if t_ch == p_ch {
                matched = true;
            }
        } else if p_ch == b'-' && prev_ch != 0 && at(pat, *p + 1) != 0 && at(pat, *p + 1) != b']' {
            // `prev_ch`..`p_ch` inclusive range.
            *p += 1;
            p_ch = at(pat, *p);
            if p_ch == b'\\' {
                *p += 1;
                p_ch = at(pat, *p);
                if p_ch == 0 {
                    return Wm::AbortAll;
                }
            }
            if t_ch <= p_ch && t_ch >= prev_ch {
                matched = true;
            }
            p_ch = 0; // makes prev_ch get set to 0 next iteration
        } else if p_ch == b'[' && at(pat, *p + 1) == b':' {
            // POSIX `[:class:]`.
            *p += 2;
            let s = *p;
            while at(pat, *p) != 0 && at(pat, *p) != b']' {
                *p += 1;
            }
            if at(pat, *p) == 0 {
                return Wm::AbortAll;
            }
            // `*p` is now on `]`; the class name is `pat[s..*p-1]` and `pat[*p-1]`
            // must be `:`. `i < 0` in git corresponds to `*p <= s` here.
            if *p <= s || pat[*p - 1] != b':' {
                // Not a real `[:class:]`: treat the inner `[` as a literal member.
                *p = s - 2;
                p_ch = b'[';
                if t_ch == p_ch {
                    matched = true;
                }
            } else {
                match class_matches(&pat[s..*p - 1], t_ch) {
                    Some(true) => matched = true,
                    Some(false) => {}
                    // Malformed `[:class:]` string.
                    None => return Wm::AbortAll,
                }
                p_ch = 0;
            }
        } else if t_ch == p_ch {
            matched = true;
        }
        // git's do-while tail: `prev_ch = p_ch, (p_ch = *++p) != ']'`.
        prev_ch = p_ch;
        *p += 1;
        p_ch = at(pat, *p);
        if p_ch == b']' {
            break;
        }
    }
    // `if (matched == negated) return WM_NOMATCH;` (the `WM_PATHNAME`/`'/'` guard
    // is inert at flags=0).
    if matched == negated {
        Wm::NoMatch
    } else {
        Wm::Match
    }
}

/// git's `wildmatch.c` POSIX character classes (`[:alpha:]`, `[:digit:]`, …),
/// evaluated for ASCII byte `c`. `None` marks a class name git rejects as a
/// malformed `[:class:]` string.
fn class_matches(name: &[u8], c: u8) -> Option<bool> {
    let m = match name {
        b"alnum" => c.is_ascii_alphanumeric(),
        b"alpha" => c.is_ascii_alphabetic(),
        b"blank" => c == b' ' || c == b'\t',
        b"cntrl" => c.is_ascii_control(),
        b"digit" => c.is_ascii_digit(),
        b"graph" => c.is_ascii_graphic(),
        b"lower" => c.is_ascii_lowercase(),
        // `isprint`: printable, space included.
        b"print" => (0x20..=0x7e).contains(&c),
        b"punct" => c.is_ascii_punctuation(),
        // C's `isspace`: space, `\t`, `\n`, `\v`, `\f`, `\r`.
        b"space" => matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'),
        b"upper" => c.is_ascii_uppercase(),
        b"xdigit" => c.is_ascii_hexdigit(),
        _ => return None,
    };
    Some(m)
}

/// Turn one gix change into a [`FileChange`], or `None` for the directory entries
/// git does not report (gix emits those *and* recurses into them).
fn prepare_change(
    repo: &gix::Repository,
    change: &ChangeDetached,
    with_counts: bool,
) -> Result<Option<FileChange>> {
    let (path, status, old, new) = match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            if entry_mode.is_tree() {
                return Ok(None);
            }
            (
                location.to_vec(),
                b'A',
                None,
                Some((*id, entry_mode.is_commit())),
            )
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            if entry_mode.is_tree() {
                return Ok(None);
            }
            (
                location.to_vec(),
                b'D',
                Some((*id, entry_mode.is_commit())),
                None,
            )
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            // A directory whose contents changed; the changed files themselves are
            // reported separately by the recursive walk.
            if entry_mode.is_tree() && previous_entry_mode.is_tree() {
                return Ok(None);
            }
            let status = if type_class(previous_entry_mode.kind()) == type_class(entry_mode.kind()) {
                b'M'
            } else {
                b'T'
            };
            (
                location.to_vec(),
                status,
                Some((*previous_id, previous_entry_mode.is_commit())),
                Some((*id, entry_mode.is_commit())),
            )
        }
        // Never produced: rewrite tracking is disabled via Options::default().
        ChangeDetached::Rewrite { .. } => bail!("rename/copy detection is not supported"),
    };

    let mut f = FileChange {
        path,
        status,
        added: 0,
        deleted: 0,
        is_binary: false,
        old_size: 0,
        new_size: 0,
    };

    if with_counts {
        let old_content = match old {
            Some((id, is_sub)) => content_of(repo, id, is_sub)?,
            None => Vec::new(),
        };
        let new_content = match new {
            Some((id, is_sub)) => content_of(repo, id, is_sub)?,
            None => Vec::new(),
        };
        f.old_size = old_content.len();
        f.new_size = new_content.len();
        f.is_binary = is_binary(&old_content) || is_binary(&new_content);
        let mode_only = matches!((old, new), (Some((a, _)), Some((b, _))) if a == b);
        if !f.is_binary && !mode_only {
            let (added, deleted) = count_changed_lines(&old_content, &new_content)?;
            f.added = added;
            f.deleted = deleted;
        }
    }
    Ok(Some(f))
}

/// git's status letters distinguish a change of file *type* (`T`) from a change of
/// contents or permissions (`M`); regular and executable files are the same type.
fn type_class(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::Tree => 0,
        EntryKind::Blob | EntryKind::BlobExecutable => 1,
        EntryKind::Link => 2,
        EntryKind::Commit => 3,
    }
}

/// The path of a change, for stable diff ordering.
fn change_path(change: &ChangeDetached) -> &[u8] {
    match change {
        ChangeDetached::Addition { location, .. }
        | ChangeDetached::Deletion { location, .. }
        | ChangeDetached::Modification { location, .. }
        | ChangeDetached::Rewrite { location, .. } => location,
    }
}

// ---------------------------------------------------------------------------
// --stat
// ---------------------------------------------------------------------------

/// git's `--stat` (`show_stats()`): a right-aligned change count and a `+`/`-` bar per
/// file, then a summary line. The geometry is git's, computed in signed arithmetic so a
/// tight width can drive an intermediate negative exactly as git's `int`s do. `sw` caps
/// the total width (`--stat-width`/`--stat=<w>`, `-1` == the 80-column non-tty terminal),
/// the name and graph columns (`--stat-name-width`/`--stat-graph-width`, `0` == uncapped),
/// and the number of listed files (`--stat-count`, `0` == all); the summary always tallies
/// every file.
fn emit_stat(out: &mut Vec<u8>, files: &[FileChange], sw: &StatWidths) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }

    // `count = options->stat_count ? options->stat_count : data->nr`: only the first
    // `count` files get a bar line, and the geometry scan stops there too.
    let mut count: i64 = if sw.count != 0 {
        sw.count
    } else {
        files.len() as i64
    };

    let mut max_len: i64 = 0;
    let mut max_change: i64 = 0;
    let mut bin_width: i64 = 0;
    let mut number_width: i64 = 0;
    let mut i: i64 = 0;
    while i < count && i < files.len() as i64 {
        let f = &files[i as usize];
        i += 1;
        max_len = max_len.max(display_width(&f.path) as i64);
        if f.is_binary {
            // `"Bin XXX -> YYY bytes"`: 14 fixed chars plus each size's decimal width.
            let w = 14 + decimal_width(f.new_size) as i64 + decimal_width(f.old_size) as i64;
            bin_width = bin_width.max(w);
            // Change counts are aligned with the literal "Bin" for binary files.
            number_width = 3;
            continue;
        }
        max_change = max_change.max((f.added + f.deleted) as i64);
    }
    count = i;

    // `stat_width == -1` means the terminal width (80 for a non-tty); an explicit `0`
    // also falls back to 80, only a positive value overrides.
    let mut width: i64 = if sw.width > 0 {
        sw.width
    } else {
        STAT_TERM_WIDTH as i64
    };
    number_width = number_width.max(decimal_width(max_change.max(0) as usize) as i64);
    let stat_name_width = if sw.name_width == -1 { 0 } else { sw.name_width };
    let stat_graph_width = if sw.graph_width == -1 { 0 } else { sw.graph_width };

    if width < 16 + 6 + number_width {
        width = 16 + 6 + number_width;
    }

    let mut graph_width = if max_change + 4 > bin_width {
        max_change
    } else {
        bin_width - 4
    };
    if stat_graph_width > 0 && stat_graph_width < graph_width {
        graph_width = stat_graph_width;
    }
    let mut name_width = if stat_name_width > 0 && stat_name_width < max_len {
        stat_name_width
    } else {
        max_len
    };

    // Fixed overhead per line is 6 columns: " ", " | ", and " " before the bar.
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

    let name_width = name_width.max(0) as usize;
    let number_width = number_width.max(0) as usize;
    let graph_width = graph_width.max(0) as usize;
    let max_change = max_change.max(0) as usize;

    // The summary line counts every file, even those past the `--stat-count` cut.
    let mut total_added = 0usize;
    let mut total_deleted = 0usize;
    for f in files {
        if !f.is_binary {
            total_added += f.added;
            total_deleted += f.deleted;
        }
    }

    for f in files.iter().take(count.max(0) as usize) {
        let (prefix, name) = elide_name(&f.path, name_width);
        let padding = name_width.saturating_sub(prefix.len() + display_width(name));
        out.push(b' ');
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&b" ".repeat(padding));
        out.extend_from_slice(b" | ");

        if f.is_binary {
            // For binaries the counts are byte sizes, not lines.
            write!(out, "{:>width$}", "Bin", width = number_width)?;
            if f.old_size == 0 && f.new_size == 0 {
                out.push(b'\n');
            } else {
                writeln!(out, " {} -> {} bytes", f.old_size, f.new_size)?;
            }
            continue;
        }

        let change = f.added + f.deleted;
        write!(out, "{change:>number_width$}")?;

        let (mut add, mut del) = (f.added, f.deleted);
        if graph_width < max_change {
            let mut total = scale_linear(add + del, graph_width, max_change);
            if total < 2 && add > 0 && del > 0 {
                total = 2;
            }
            if add < del {
                add = scale_linear(add, graph_width, max_change);
                del = total.saturating_sub(add);
            } else {
                del = scale_linear(del, graph_width, max_change);
                add = total.saturating_sub(del);
            }
        }
        if add > 0 || del > 0 {
            out.push(b' ');
            out.extend_from_slice(&b"+".repeat(add));
            out.extend_from_slice(&b"-".repeat(del));
        }
        out.push(b'\n');
    }

    // `--stat-count` cut off some files: git prints a bare " ..." continuation line.
    if (count as usize) < files.len() {
        out.extend_from_slice(b" ...\n");
    }

    write_stat_summary(out, files.len(), total_added, total_deleted)
}

/// git's `--stat`/`--shortstat` summary line: ` N files changed, A insertions(+),
/// D deletions(-)`, with the `insertions`/`deletions` clauses appearing on git's
/// same conditions.
fn write_stat_summary(
    out: &mut Vec<u8>,
    n: usize,
    total_added: usize,
    total_deleted: usize,
) -> Result<()> {
    write!(out, " {n} file{} changed", if n == 1 { "" } else { "s" })?;
    if total_added > 0 || total_deleted == 0 {
        write!(
            out,
            ", {total_added} insertion{}(+)",
            if total_added == 1 { "" } else { "s" }
        )?;
    }
    if total_deleted > 0 || total_added == 0 {
        write!(
            out,
            ", {total_deleted} deletion{}(-)",
            if total_deleted == 1 { "" } else { "s" }
        )?;
    }
    out.push(b'\n');
    Ok(())
}

/// git's `--numstat`: `<added>\t<deleted>\t<path>` per file, with `-\t-` for a
/// binary file whose line counts are undefined.
fn emit_numstat(out: &mut Vec<u8>, files: &[FileChange]) {
    for f in files {
        if f.is_binary {
            out.extend_from_slice(b"-\t-\t");
        } else {
            out.extend_from_slice(format!("{}\t{}\t", f.added, f.deleted).as_bytes());
        }
        out.extend_from_slice(&f.path);
        out.push(b'\n');
    }
}

/// git's `--shortstat`: the `--stat` summary line only. Binary files contribute
/// nothing to the insertion/deletion totals, exactly as the full stat block.
fn emit_shortstat(out: &mut Vec<u8>, files: &[FileChange]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let mut total_added = 0usize;
    let mut total_deleted = 0usize;
    for f in files {
        if f.is_binary {
            continue;
        }
        total_added += f.added;
        total_deleted += f.deleted;
    }
    write_stat_summary(out, files.len(), total_added, total_deleted)
}

/// Scale `it` into `width` columns, guaranteeing at least one column for any
/// non-zero value — git widens by one and adds it back for exactly that reason.
fn scale_linear(it: usize, width: usize, max_change: usize) -> usize {
    if it == 0 || max_change == 0 {
        return 0;
    }
    1 + (it * width.saturating_sub(1) / max_change)
}

/// Shorten an over-long path the way git does: a `...` prefix, cut back to a
/// directory boundary when one falls inside the retained tail. A name column
/// narrower than the 3-column `...` prefix keeps only the prefix (git prints "...").
fn elide_name(path: &[u8], name_width: usize) -> (&'static str, &[u8]) {
    if display_width(path) <= name_width {
        return ("", path);
    }
    let keep = name_width.saturating_sub(3);
    let mut tail = &path[path.len().saturating_sub(keep)..];
    if let Some(slash) = tail.iter().position(|&b| b == b'/') {
        tail = &tail[slash..];
    }
    ("...", tail)
}

fn decimal_width(mut n: usize) -> usize {
    let mut w = 1;
    while n >= 10 {
        n /= 10;
        w += 1;
    }
    w
}

/// Approximate display width. Paths are treated as UTF-8 and counted in `char`s,
/// which matches git for everything but wide and combining characters.
fn display_width(path: &[u8]) -> usize {
    String::from_utf8_lossy(path).chars().count()
}

// ---------------------------------------------------------------------------
// --graph
// ---------------------------------------------------------------------------

/// The palette `--graph` paints its branch lines with, in git's `column_colors`
/// layout: the drawing colors followed by the reset that terminates each of them,
/// so the last entry is both "the reset" and the sentinel index meaning "uncolored".
///
/// `git help config` calls the knob `log.graphColors`; git's `parse_graph_colors_config`
/// splits it on commas, keeps the specs it can parse, and warns about the rest.
fn graph_colors(repo: &gix::Repository) -> Vec<String> {
    const RESET: &str = "\x1b[m";
    let Some(spec) = repo.config_snapshot().string("log.graphColors") else {
        // git's `column_colors_ansi`.
        return [
            "\x1b[31m", "\x1b[32m", "\x1b[33m", "\x1b[34m", "\x1b[35m", "\x1b[36m", "\x1b[1;31m",
            "\x1b[1;32m", "\x1b[1;33m", "\x1b[1;34m", "\x1b[1;35m", "\x1b[1;36m", RESET,
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    };
    let spec = spec.to_string();
    let mut colors: Vec<String> = Vec::new();
    for word in spec.split(',') {
        match super::color::parse_color_spec(word) {
            Some(code) => colors.push(code),
            None => eprintln!("warning: ignored invalid color '{word}' in log.graphColors"),
        }
    }
    colors.push(RESET.to_string());
    colors
}

/// Prefix every line of every commit's block with git's ASCII graph, flushing the
/// merge and collapse rows that fall between commits.
fn render_graph(nodes: &[Node], blocks: &[Vec<u8>], colors: Vec<String>, want_color: bool) -> Result<Vec<u8>> {
    let mut graph = Graph::new(colors, want_color);
    let mut out: Vec<u8> = Vec::new();

    for (i, node) in nodes.iter().enumerate() {
        // A boundary commit is where the drawn history stops: git never adds its
        // parents to the graph, so its column closes and the rows under it are
        // blank rather than a continuing `|`.
        let drawn_parents: &[ObjectId] = if node.boundary { &[] } else { &node.parents };
        graph.update(node.id, drawn_parents, node.boundary);

        let block = &blocks[i];
        let ends_nl = block.ends_with(b"\n");
        let mut lines: Vec<&[u8]> = block.split(|&b| b == b'\n').collect();
        if ends_nl {
            lines.pop();
        }

        for (j, line) in lines.iter().enumerate() {
            out.extend_from_slice(&graph.next_line());
            out.extend_from_slice(line);
            if ends_nl || j + 1 < lines.len() {
                out.push(b'\n');
            }
        }

        // Rows the commit's own text did not consume: the `|\` of a merge and the
        // `|/` of a collapse both appear on lines of their own. A collapse needs at
        // most one row per column, so the bound below can only trip on a bug here —
        // failing beats hanging the caller.
        let mut guard = graph.columns.len() + graph.new_columns.len() + 8;
        while graph.state != GraphState::Padding {
            out.extend_from_slice(&graph.next_line());
            out.push(b'\n');
            guard -= 1;
            if guard == 0 {
                bail!("--graph failed to settle the commit graph");
            }
        }
    }
    Ok(out)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GraphState {
    Padding,
    Commit,
    PostMerge,
    Collapsing,
}

/// A branch line and the palette index it is drawn in — git's `struct column`.
#[derive(Clone, Copy)]
struct GraphColumn {
    id: ObjectId,
    color: usize,
}

/// A row under construction. The visible width is tracked separately from the
/// buffer because the color escapes occupy bytes but no columns, and every row of
/// a commit is padded to the same *visible* width so the text to its right aligns.
struct GraphLine {
    buf: Vec<u8>,
    width: usize,
}

impl GraphLine {
    fn new() -> Self {
        GraphLine { buf: Vec::new(), width: 0 }
    }

    fn addch(&mut self, c: u8) {
        self.buf.push(c);
        self.width += 1;
    }

    /// Every graph row for one commit is the same width, so text to its right lines up.
    fn pad_to(&mut self, width: usize) {
        while self.width < width {
            self.addch(b' ');
        }
    }
}

/// git's `graph.c` column state machine, for commits with at most two parents.
struct Graph {
    /// Columns as of the previous commit.
    columns: Vec<GraphColumn>,
    /// Columns as of the current commit.
    new_columns: Vec<GraphColumn>,
    /// Screen-slot to new-column index, `-1` for an empty slot.
    mapping: Vec<i32>,
    old_mapping: Vec<i32>,
    commit: ObjectId,
    /// `--boundary`: the current commit is an excluded ancestor, drawn `o`.
    boundary: bool,
    /// The current commit's parents, in order — the post-merge row draws one edge
    /// per parent and takes each edge's color from that parent's column.
    parents: Vec<ObjectId>,
    num_parents: usize,
    width: usize,
    state: GraphState,
    prev_state: GraphState,
    /// `log.graphColors`, with the reset appended; the last index means "uncolored".
    colors: Vec<String>,
    /// The color the next column to be opened is assigned, cycling through `colors`.
    default_column_color: usize,
    want_color: bool,
}

impl Graph {
    fn new(colors: Vec<String>, want_color: bool) -> Self {
        // git starts one short of the wrap point, because the first column opened
        // always increments first — which lands the first branch line on index 0.
        let default_column_color = colors.len().saturating_sub(2);
        Graph {
            boundary: false,
            columns: Vec::new(),
            new_columns: Vec::new(),
            mapping: Vec::new(),
            old_mapping: Vec::new(),
            commit: ObjectId::null(gix::hash::Kind::Sha1),
            parents: Vec::new(),
            num_parents: 0,
            width: 0,
            state: GraphState::Padding,
            prev_state: GraphState::Padding,
            colors,
            default_column_color,
            want_color,
        }
    }

    /// The index that means "emit no escapes" — git's `column_colors_max`, which is
    /// also where the reset lives.
    fn uncolored(&self) -> usize {
        self.colors.len() - 1
    }

    /// git's `graph_get_current_column_color`: the color a newly opened column takes,
    /// or the uncolored sentinel when this run is not coloring at all.
    fn current_column_color(&self) -> usize {
        if self.want_color {
            self.default_column_color
        } else {
            self.uncolored()
        }
    }

    fn increment_column_color(&mut self) {
        self.default_column_color = (self.default_column_color + 1) % self.uncolored();
    }

    /// git's `graph_find_commit_color`: a commit that already owns a column keeps its
    /// color across the row, so a branch line does not change color as it descends.
    fn commit_color(&self, id: ObjectId) -> usize {
        self.columns
            .iter()
            .find(|c| c.id == id)
            .map_or_else(|| self.current_column_color(), |c| c.color)
    }

    /// Draw one branch-line character in its column's color — git's
    /// `graph_line_write_column`.
    fn write_column(&self, line: &mut GraphLine, col: &GraphColumn, ch: u8) {
        let uncolored = self.uncolored();
        if col.color < uncolored {
            line.buf.extend_from_slice(self.colors[col.color].as_bytes());
        }
        line.addch(ch);
        if col.color < uncolored {
            line.buf.extend_from_slice(self.colors[uncolored].as_bytes());
        }
    }

    fn update(&mut self, id: ObjectId, parents: &[ObjectId], boundary: bool) {
        self.commit = id;
        self.boundary = boundary;
        self.parents = parents.to_vec();
        self.num_parents = parents.len();
        self.update_columns(parents);
        // Every commit's rows are fully flushed before the next one starts, so
        // the skip and pre-commit states git needs for interrupted output and
        // octopus expansion never arise here.
        self.state = GraphState::Commit;
    }

    fn update_columns(&mut self, parents: &[ObjectId]) {
        std::mem::swap(&mut self.columns, &mut self.new_columns);
        self.new_columns.clear();

        let num_columns = self.columns.len();
        let max_new_columns = num_columns + self.num_parents;
        self.mapping = vec![-1i32; 2 * max_new_columns.max(1)];

        let mut seen_this = false;
        let mut mapping_idx = 0usize;
        let mut is_commit_in_columns = true;
        let mut i = 0usize;
        while i <= num_columns {
            let col_commit = if i == num_columns {
                if seen_this {
                    break;
                }
                is_commit_in_columns = false;
                self.commit
            } else {
                self.columns[i].id
            };

            if col_commit == self.commit {
                let old_mapping_idx = mapping_idx;
                seen_this = true;
                for parent in parents {
                    // A merge fans out, and a commit no column was following starts a
                    // fresh line: both open a lane that gets the next color in the cycle.
                    if self.num_parents > 1 || !is_commit_in_columns {
                        self.increment_column_color();
                    }
                    self.insert_column(*parent, &mut mapping_idx);
                }
                // A commit occupies at least two screen slots even with no parents.
                if mapping_idx == old_mapping_idx {
                    mapping_idx += 2;
                }
            } else {
                self.insert_column(col_commit, &mut mapping_idx);
            }
            i += 1;
        }

        while self.mapping.len() > 1 && *self.mapping.last().unwrap_or(&0) < 0 {
            self.mapping.pop();
        }

        // Every row of this commit is padded to the widest row it can produce.
        let mut max_cols = num_columns + self.num_parents;
        if self.num_parents < 1 {
            max_cols += 1;
        }
        if is_commit_in_columns && max_cols > 0 {
            max_cols -= 1;
        }
        self.width = max_cols * 2;
    }

    fn mapping_correct(&self) -> bool {
        self.mapping
            .iter()
            .enumerate()
            .all(|(i, &t)| t < 0 || t == (i as i32) / 2)
    }

    fn next_line(&mut self) -> Vec<u8> {
        match self.state {
            GraphState::Commit => self.commit_line(),
            GraphState::PostMerge => self.post_merge_line(),
            GraphState::Collapsing => self.collapsing_line(),
            GraphState::Padding => {
                let mut line = GraphLine::new();
                for col in &self.new_columns {
                    self.write_column(&mut line, col, b'|');
                    line.addch(b' ');
                }
                line.pad_to(self.width);
                line.buf
            }
        }
    }

    fn commit_line(&mut self) -> Vec<u8> {
        let mut line = GraphLine::new();
        let mut seen_this = false;
        let num_columns = self.columns.len();
        let mut i = 0usize;
        while i <= num_columns {
            let col_commit = if i == num_columns {
                if seen_this {
                    break;
                }
                self.commit
            } else {
                self.columns[i].id
            };

            if col_commit == self.commit {
                seen_this = true;
                // `graph_output_commit_char()`: a boundary commit is drawn as a
                // hollow `o` rather than the usual `*`.
                line.addch(if self.boundary { b'o' } else { b'*' });
            } else if seen_this && self.num_parents > 1 {
                self.write_column(&mut line, &self.columns[i], b'\\');
            } else if self.prev_state == GraphState::Collapsing
                && self.old_mapping.get(2 * i + 1).copied().unwrap_or(-1) == i as i32
                && self.mapping.get(2 * i).copied().unwrap_or(-1) < i as i32
            {
                self.write_column(&mut line, &self.columns[i], b'/');
            } else {
                self.write_column(&mut line, &self.columns[i], b'|');
            }
            line.addch(b' ');
            i += 1;
        }
        line.pad_to(self.width);
        let line = line.buf;

        self.prev_state = GraphState::Commit;
        self.state = if self.num_parents > 1 {
            GraphState::PostMerge
        } else if self.mapping_correct() {
            GraphState::Padding
        } else {
            GraphState::Collapsing
        };
        line
    }

    fn post_merge_line(&mut self) -> Vec<u8> {
        let mut line = GraphLine::new();
        let mut seen_this = false;
        let num_columns = self.columns.len();
        let mut i = 0usize;
        while i <= num_columns {
            let col_commit = if i == num_columns {
                if seen_this {
                    break;
                }
                self.commit
            } else {
                self.columns[i].id
            };

            if col_commit == self.commit {
                seen_this = true;
                // One edge per parent, each in the color of the lane that parent
                // just took — git's `graph_output_post_merge_line`, which looks the
                // parent up in `new_columns` for exactly this reason.
                for (n, parent) in self.parents.clone().into_iter().enumerate() {
                    let ch = if n == 0 { b'|' } else { b'\\' };
                    match self.new_columns.iter().position(|c| c.id == parent) {
                        Some(p) => self.write_column(&mut line, &self.new_columns[p], ch),
                        None => line.addch(ch),
                    }
                }
            } else if seen_this {
                self.write_column(&mut line, &self.columns[i], b'\\');
                line.addch(b' ');
            } else {
                self.write_column(&mut line, &self.columns[i], b'|');
                line.addch(b' ');
            }
            i += 1;
        }
        line.pad_to(self.width);
        let line = line.buf;

        self.prev_state = GraphState::PostMerge;
        self.state = if self.mapping_correct() {
            GraphState::Padding
        } else {
            GraphState::Collapsing
        };
        line
    }

    fn collapsing_line(&mut self) -> Vec<u8> {
        std::mem::swap(&mut self.mapping, &mut self.old_mapping);
        let size = self.old_mapping.len();
        self.mapping = vec![-1i32; size];

        let mut horizontal_edge: i32 = -1;
        let mut horizontal_edge_target: i32 = -1;

        for i in 0..size {
            let target = self.old_mapping[i];
            if target < 0 {
                continue;
            }
            if (target as usize) * 2 == i {
                // Already where it belongs.
                self.mapping[i] = target;
            } else if i >= 1 && self.mapping[i - 1] < 0 {
                // Nothing to the left: step one slot over.
                self.mapping[i - 1] = target;
                if horizontal_edge == -1 {
                    horizontal_edge = i as i32;
                    horizontal_edge_target = target;
                    let mut j = (target as usize) * 2 + 3;
                    while (j as i64) < i as i64 - 2 {
                        self.mapping[j] = target;
                        j += 2;
                    }
                }
            } else if i >= 1 && self.mapping[i - 1] == target {
                // Shares a parent with the line to its left; already drawn.
            } else if i >= 2 {
                // Cross over the unrelated line to the left.
                self.mapping[i - 2] = target;
            }
        }

        if size > 0 && self.mapping[size - 1] < 0 {
            self.mapping.pop();
        }

        let mut line = GraphLine::new();
        let mut used_horizontal = false;
        for i in 0..self.mapping.len() {
            let target = self.mapping[i];
            // A collapsing edge is drawn in the color of the lane it is heading for,
            // which is the new column the mapping points at.
            let col = usize::try_from(target).ok().and_then(|t| self.new_columns.get(t)).copied();
            let Some(col) = col else {
                line.addch(b' ');
                continue;
            };
            if (target as usize) * 2 == i {
                self.write_column(&mut line, &col, b'|');
            } else if target == horizontal_edge_target && i as i32 != horizontal_edge - 1 {
                if i != (target as usize) * 2 + 3 {
                    self.mapping[i] = -1;
                }
                used_horizontal = true;
                self.write_column(&mut line, &col, b'_');
            } else {
                if used_horizontal && (i as i32) < horizontal_edge {
                    self.mapping[i] = -1;
                }
                self.write_column(&mut line, &col, b'/');
            }
        }
        line.pad_to(self.width);
        let line = line.buf;

        self.prev_state = GraphState::Collapsing;
        if self.mapping_correct() {
            self.state = GraphState::Padding;
        }
        line
    }
}

impl Graph {
    /// Record `id` in the new column list (reusing its column when it is already
    /// there) and point the next screen slot at it. A column opened here takes the
    /// color `id` already had elsewhere, or the current one — git's
    /// `graph_insert_into_new_columns`.
    fn insert_column(&mut self, id: ObjectId, mapping_idx: &mut usize) {
        let col = match self.new_columns.iter().position(|c| c.id == id) {
            Some(i) => i,
            None => {
                let color = self.commit_color(id);
                self.new_columns.push(GraphColumn { id, color });
                self.new_columns.len() - 1
            }
        };
        if let Some(slot) = self.mapping.get_mut(*mapping_idx) {
            *slot = col as i32;
        }
        *mapping_idx += 2;
    }
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

/// The `--date=` output modes this port renders byte-for-byte, plus `relative`,
/// which is measured against the current wall clock. The remaining process-time /
/// zone-dependent modes (`human`, `local`) are still rejected rather than faked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DateMode {
    /// git's `DATE_NORMAL`: `Www Mmm D HH:MM:SS YYYY +ZZZZ`.
    Default,
    /// `short`: `YYYY-MM-DD`.
    Short,
    /// `iso`/`iso8601`: `YYYY-MM-DD HH:MM:SS +ZZZZ`.
    Iso,
    /// `iso-strict`/`iso8601-strict`: `YYYY-MM-DDTHH:MM:SS+ZZ:ZZ`.
    IsoStrict,
    /// `rfc`/`rfc2822`: `Www, D Mmm YYYY HH:MM:SS +ZZZZ`.
    Rfc,
    /// `unix`: the raw epoch seconds, no timezone.
    Unix,
    /// `raw`: `<seconds> +ZZZZ`.
    Raw,
    /// `relative`: `N <unit> ago`, measured against the current time.
    Relative,
}

/// `--color=<when>` (and `--color`/`--no-color`): whether `%C`/`%d` emit ANSI.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorWhen {
    Always,
    Never,
    /// Color when stdout is a terminal (or we are paging to one).
    Auto,
}

/// Map a `--date=` value to a [`DateMode`]. `None` for a value git accepts but
/// this port renders time/zone-dependently (surfaced terse) or does not know.
fn parse_date_mode(spec: &str) -> Option<DateMode> {
    Some(match spec {
        "default" | "normal" => DateMode::Default,
        "short" => DateMode::Short,
        "iso" | "iso8601" => DateMode::Iso,
        "iso-strict" | "iso8601-strict" => DateMode::IsoStrict,
        "rfc" | "rfc2822" => DateMode::Rfc,
        "unix" => DateMode::Unix,
        "raw" => DateMode::Raw,
        "relative" => DateMode::Relative,
        _ => return None,
    })
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Format a timestamp in the requested [`DateMode`], matching git byte-for-byte.
fn format_date(seconds: i64, offset: i32, mode: DateMode) -> String {
    match mode {
        DateMode::Default => format_git_date(seconds, offset),
        // Relative dates need the current time; callers route them through
        // `fmt_time`, but keep this arm self-contained rather than unreachable.
        DateMode::Relative => format_relative(seconds, now_secs()),
        DateMode::Unix => format!("{seconds}"),
        DateMode::Raw => {
            let (sign, off) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
            format!("{seconds} {sign}{:02}{:02}", off / 3600, (off % 3600) / 60)
        }
        DateMode::Short | DateMode::Iso | DateMode::IsoStrict | DateMode::Rfc => {
            let local = seconds + offset as i64;
            let days = local.div_euclid(86_400);
            let secs = local.rem_euclid(86_400);
            let (hour, min, sec) = (secs / 3600, (secs % 3600) / 60, secs % 60);
            let weekday = ((days.rem_euclid(7)) + 4).rem_euclid(7) as usize;
            let (year, month, day) = civil_from_days(days);
            let (sign, off) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
            let (oh, om) = (off / 3600, (off % 3600) / 60);
            match mode {
                DateMode::Short => format!("{year}-{month:02}-{day:02}"),
                DateMode::Iso => format!(
                    "{year}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02} {sign}{oh:02}{om:02}"
                ),
                // git renders a zero UTC offset as `Z` in iso-strict (RFC 3339),
                // not `+00:00` (verified against git 2.55).
                DateMode::IsoStrict => {
                    let tz = if offset == 0 {
                        "Z".to_string()
                    } else {
                        format!("{sign}{oh:02}:{om:02}")
                    };
                    format!("{year}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}{tz}")
                }
                DateMode::Rfc => format!(
                    "{}, {day} {} {year} {hour:02}:{min:02}:{sec:02} {sign}{oh:02}{om:02}",
                    WEEKDAYS[weekday],
                    MONTHS[(month - 1) as usize],
                ),
                _ => unreachable!(),
            }
        }
    }
}

/// Format a commit time exactly like stock `git log`'s default (`DATE_NORMAL`)
/// mode: `Www Mmm <day> HH:MM:SS YYYY +ZZZZ`, in the commit's own timezone
/// offset. The day is **unpadded** — git's `show_date` builds this with a bare
/// `%d` (printf integer), so a single-digit day gets one space, not two
/// (verified against git 2.55: `Mon Jan 2 ...`, not `Mon Jan  2 ...`).
fn format_git_date(seconds: i64, offset: i32) -> String {
    // Shift into the commit's local wall-clock time, then split into whole days
    // (since the Unix epoch) and the seconds within the day. `div_euclid` /
    // `rem_euclid` keep the split correct for pre-1970 (negative) timestamps.
    let local = seconds + offset as i64;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (hour, min, sec) = (secs / 3600, (secs % 3600) / 60, secs % 60);

    // 1970-01-01 (day 0) was a Thursday, index 4 with Sunday = 0.
    let weekday = ((days.rem_euclid(7)) + 4).rem_euclid(7) as usize;
    let (year, month, day) = civil_from_days(days);

    let (sign, off) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
    let (off_h, off_m) = (off / 3600, (off % 3600) / 60);

    format!(
        "{} {} {} {:02}:{:02}:{:02} {} {}{:02}{:02}",
        WEEKDAYS[weekday],
        MONTHS[(month - 1) as usize],
        day,
        hour,
        min,
        sec,
        year,
        sign,
        off_h,
        off_m,
    )
}

/// Convert a day count since the Unix epoch into a civil `(year, month, day)`,
/// month and day 1-based. Howard Hinnant's `civil_from_days` algorithm, which is
/// exact for the whole representable range and needs no calendar tables.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month as u32, day)
}

/// Strip trailing whitespace (git trims a subject line this way).
fn trim_end_ws(mut s: &[u8]) -> &[u8] {
    while let Some(&last) = s.last() {
        if last == b'\n' || last == b'\r' || last == b' ' || last == b'\t' {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every expectation below was verified against stock `git log -- <spec>` on a
    // real repository with git 2.55.0: a bracket pathspec is a wildcard pathspec,
    // so it never gets the literal leading-directory shortcut, and its `[…]`
    // expression follows git's `wildmatch.c:dowild` (flags=0) rules.
    fn m(spec: &str, path: &[u8]) -> bool {
        pathspec_matches(spec, path).unwrap()
    }

    #[test]
    fn bracket_set_matches_a_listed_char() {
        // `git log -- 'READM[Ee]'` shows the README commit; the set picks `E`.
        assert!(m("READM[Ee]", b"README"));
        assert!(m("f[oi]le", b"file"));
        assert!(m("f[oi]le", b"fole"));
        // `x` is not in the set, so no match.
        assert!(!m("f[oi]le", b"fxle"));
    }

    #[test]
    fn bracket_range() {
        // `READM[A-Z]` matches (E in A-Z); `READM[a-d]` does not (E not in a-d).
        assert!(m("READM[A-Z]", b"README"));
        assert!(!m("READM[a-d]", b"README"));
    }

    #[test]
    fn bracket_negation_both_forms() {
        // `[!x]`/`[^x]` match `E` (not `x`); `[!E]` rejects `E`.
        assert!(m("READM[!x]", b"README"));
        assert!(m("READM[^x]", b"README"));
        assert!(!m("READM[!E]", b"README"));
    }

    #[test]
    fn posix_character_class() {
        // `[[:upper:]]` matches `E`; `[[:digit:]]` does not.
        assert!(m("READM[[:upper:]]", b"README"));
        assert!(!m("READM[[:digit:]]", b"README"));
    }

    #[test]
    fn malformed_bracket_is_no_match() {
        // git prints nothing for an unterminated class (WM_ABORT_ALL → no-match).
        assert!(!m("READM[Ee", b"README"));
    }

    #[test]
    fn star_spans_slashes_and_bracket_dir_needs_full_match() {
        // flags=0: `*` spans `/`, so `builtin*log.c` matches `builtin/log.c`.
        assert!(m("builtin*log.c", b"builtin/log.c"));
        // A wildcard pathspec that names a directory gets no leading-dir shortcut,
        // and wildmatch leaves the trailing `/log.c` unmatched — git shows nothing.
        assert!(!m("buil[dt]in", b"builtin/log.c"));
    }

    #[test]
    fn magic_pathspec_still_surfaced_as_floor() {
        // Magic pathspecs remain unported (an honest error, never a wrong match).
        assert!(pathspec_matches(":(glob)foo", b"foo").is_err());
        assert!(pathspec_matches(":!foo", b"foo").is_err());
    }
}
