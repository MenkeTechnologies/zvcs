use anyhow::{bail, Result};
use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::{BStr, ByteSlice};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};
use gix::hash::ObjectId;
use gix::object::tree::diff::ChangeDetached;
use gix::objs::tree::EntryKind;
use gix::objs::{Kind, TreeRefIter};
use gix::prelude::ObjectIdExt;
use gix::revision::plumbing::Spec as RevSpec;

use super::filespec::{content_of, count_changed_lines_ws, is_binary};
use super::line_log;
use super::log::{DecorateStyle, Decorations, Mailmap};
use super::pretty_pad::{FlushType, PadState, WrapState};

/// git's `MINIMUM_ABBREV`: the shortest id `--abbrev=<n>` may ask for, and the width
/// the all-zero side of an `index`/raw line is padded to when nothing longer is set.
const MINIMUM_ABBREV: usize = 4;

/// git's `DEFAULT_ABBREV`, the length a valueless `--abbrev` selects.
const DEFAULT_ABBREV: usize = 7;

/// The terminal width git assumes for `--stat` when stdout is not a terminal.
const STAT_TERM_WIDTH: usize = 80;

/// `git show` — show one or more objects (commit, tree, blob, or annotated tag).
///
/// Implemented invocation forms:
///   * `git show [<commit>]`  → a commit header in the selected pretty format,
///     followed by the selected diff output against the first parent (root commits
///     diff against the empty tree).
///   * `git show <blob>`      → the raw blob bytes.
///   * `git show <tree>`      → `tree <name-as-given>` then the top-level entry
///     names, directories suffixed with `/`.
///   * `git show <tag>`       → the annotated-tag header, then the object it points to.
///
/// Pretty formats: the default `medium`, `--oneline`, and `--format=`/`--pretty=`
/// with the placeholder subset listed in [`expand_format`]. Any other placeholder
/// is rejected rather than silently dropped.
///
/// Header decoration and identity flags, shared with `git log` and rendered by
/// its code so the two commands emit identical bytes:
///   * `--decorate[=short|full|auto|no]` / `--no-decorate`, defaulting to
///     `log.decorate` and then to `auto`, with `--decorate-refs=<pattern>`,
///     `--decorate-refs-exclude=<pattern>`, `--clear-decorations`,
///     `log.excludeDecoration` and `log.initialDecorationSet` filtering which
///     refs may decorate
///   * `--use-mailmap`/`--mailmap` (and their `--no-` forms), defaulting to
///     `log.mailmap` (true), which resolves the `Author:` line through `.mailmap`
///   * `--source` / `--no-source`, annotating the header with the argument the
///     commit was reached from — the endpoint token for a range, with git's
///     parent-inheritance across a walk
///
/// `-L<start>,<end>:<file>` (and its `+<n>`/`-<n>`/`/<regex>/`/`:<funcname>`
/// spellings, repeatable) selects git's line-level history — the same machinery
/// `git log -L` uses, since `cmd_show` shares `cmd_log_init_finish`. Given a range
/// argument it becomes a walk and behaves exactly like `git log -L`; given a single
/// commit it keeps git's `no_walk` shape, where the pending commit's parents are
/// never parsed and the tracked ranges therefore print as a brand-new file (a merge
/// prints its header alone). See [`super::line_log`].
///
/// Diff output formats: `-p`/`--patch`, `--stat`, `--shortstat`, `--numstat`, `--raw`,
/// `--summary`, `--name-only`, `--name-status`, `-s`/`--no-patch`, `-q`/`--quiet`, and
/// the two combining `OPT_BITOP` spellings `--patch-with-stat` / `--patch-with-raw`,
/// which are exactly `-p --stat` and `-p --raw`.
/// Their interaction is git's, reproduced in [`Formats`]; `-q`/`--quiet` suppresses the
/// default patch but yields to any explicit format flag regardless of position (git
/// applies its NO_OUTPUT bit before the other diff flags parse). `--abbrev[=<n>]` and
/// `--no-abbrev` set the width of the ids the raw columns and the patch `index` line
/// carry, applied as a `core.abbrev` override — `--no-abbrev` is git's zero, so the raw
/// columns widen to the whole hash while the `index` line keeps the configured default.
///
/// The commit message is reprinted under a four-space indent with its tabs expanded
/// against the message's own left edge — git's `expand_tabs_in_log`, 8 by default —
/// so whatever the author lined up in columns survives the indent.
/// `--expand-tabs[=<n>]` and `--no-expand-tabs` change or disable that.
///
/// The patch uses git's default settings: Myers diff with the indent (slider)
/// heuristic, three lines of context, `@@`-hunk function-context, binary-file
/// detection, and the `\ No newline at end of file` marker. Output is never
/// colorized (equivalent to `git --no-color show` / a non-tty pipe).
///
/// Deviations, surfaced rather than faked:
///   * Non-ASCII/special paths are C-quoted the way `write_name_quoted()` quotes them,
///     but `core.quotePath` itself is not consulted, and `--stat` measures a path in
///     `char`s rather than display columns.
///   * `--stat` assumes an 80-column terminal (`COLUMNS` is not consulted), but the
///     `--stat-width`/`--stat=<w>`, `--stat-name-width`, `--stat-graph-width`, and
///     `--stat-count` flags and the `diff.statNameWidth`/`diff.statGraphWidth` config
///     are honored (flag over config over the 80-column / uncapped default).
///
/// Revision arguments accept the full walk grammar: plain names are shown directly
/// (deduplicated per commit, in argument order), while anything that excludes drives a
/// revision walk instead — `cmd_show` starts with `rev.no_walk = 1` and
/// `add_pending_object_with_path()` clears it as soon as a pending object carries
/// `UNINTERESTING`. That covers `^a`, the left side of a range (`a..b`), the merge
/// bases of a symmetric difference (`a...b`), the parents of `a^!`, and `--not`,
/// which flips the sense of every revision after it (and is undone by a second
/// `--not`). Under a walk the pending objects are peeled through their tag chain and
/// anything that is not a commit — a tree, a blob — contributes nothing, while `a^@`
/// stays a no-walk record of the parents themselves. The walk itself is `git log`'s
/// (see [`super::log::walk`]), because `cmd_show` hands its pending list to
/// `cmd_log_walk`: a commit-date-ordered frontier whose ties break by the order tips
/// and parents entered it, reordered by `--topo-order`/`--date-order` and reversed by
/// `--reverse`. Pathspecs after `--` limit each commit's diff by plain path prefix
/// (pathspec magic is not interpreted).
///
/// The ref-selecting pseudo-options are accepted too — `--all`, `--branches`,
/// `--tags`, `--remotes` (each optionally `=<glob>`), `--glob=<glob>` and the
/// `--exclude=<glob>` patterns the next of them consumes — pending their refs at the
/// argument position they were written at, and counting as `rev_input_given` so the
/// implicit `HEAD` is not added on top. `--no-walk[=(sorted|unsorted)]` and
/// `--do-walk` set and clear `no_walk` positionally against the revision arguments
/// that clear it; `unsorted_input` never reaches `cmd_show`'s own pending loop, so
/// `sorted` and `unsorted` are indistinguishable here. Every flag not listed above is
/// rejected explicitly.
pub fn show(args: &[String]) -> Result<ExitCode> {
    let mut specs: Vec<&str> = Vec::new();
    // `--stdin`: further revisions, one per line, read after the command line is
    // scanned. The JetBrains client uses it to ask about a batch of commits.
    let mut read_stdin = false;
    // Owns the lines so `specs` can borrow them alongside the argument slices.
    let stdin_text: String;
    let mut pathspecs: Vec<Vec<u8>> = Vec::new();
    let mut formats = Formats::default();
    // `-z` (`diffopt.line_termination = 0`): NUL-terminated records with raw paths.
    let mut z = false;
    let mut pretty = Pretty::Medium;
    // `rev->pretty_given`, which is what decides whether notes show by default.
    let mut pretty_given = false;
    let mut notes_opt = super::notes::DisplayOpt::default();
    let mut after_dashdash = false;
    // `setup_revisions()`'s `seen_dashdash`, which it establishes in a scan of
    // the whole argument vector *before* it resolves anything — so it is in
    // force for the arguments in front of the separator as well, unlike
    // `after_dashdash` above.
    let seen_dashdash = args.iter().any(|a| a == "--");
    // `--not` (`setup_revisions`' `flags ^= UNINTERESTING | BOTTOM`): a toggle that
    // reverses the sense of every revision after it, so `--not A` excludes `A` and a
    // second `--not` restores the positive reading. Recorded per revision in
    // `spec_negated`, since the toggle's state at the time a token is read is what
    // decides that token's side.
    let mut negate_revs = false;
    let mut spec_negated: Vec<bool> = Vec::new();
    // Display config shared with `git log`, overridable on the command line. The
    // config defaults are resolved after the repo is discovered; these hold the
    // CLI overrides in the meantime (`None` = fall back to config).
    let mut cli_abbrev: Option<bool> = None;
    // `--abbrev[=<n>]` (`revs->abbrev`), clamped and applied after the repo is open;
    // `--no-abbrev` is git's zero, which prints whole ids everywhere but the `index`
    // line.
    let mut abbrev_len: Option<usize> = None;
    let mut abbrev_raw: Option<String> = None;
    let mut no_abbrev = false;
    let mut cli_date: Option<DateMode> = None;
    let mut force_root = false;
    let mut first_parent = false;
    // `-q`/`--quiet`: git pre-sets DIFF_FORMAT_NO_OUTPUT before `setup_revisions`
    // parses the other diff-format flags, so it is position-independent (an explicit
    // `-p`/`--stat`/… overrides it) rather than order-sensitive like `-s`. Tracked
    // as its own flag (last `--quiet`/`--no-quiet` wins) and folded into `no_output`
    // after parsing.
    let mut quiet = false;
    // Pickaxe search (`-S<string>` / `-G<regex>`), which limits the shown diff to
    // the file pairs whose change text matches — git-fuzzy's in-commit search uses
    // `-G <query>`. `pending_pickaxe` holds the kind while the separate value form
    // (`-G` then the query in the next argv token) waits for that value.
    let mut pickaxe_s: Option<String> = None;
    let mut pickaxe_g: Option<String> = None;
    let mut pending_pickaxe: Option<char> = None;
    // `--stat` width geometry; seeded from config after repo discovery, then any
    // explicit `--stat*` flag below wins (git precedence).
    let mut stat_widths = StatWidths::default();
    // The patch-shaping options, handed to the shared renderer.
    let mut patch_opts = super::diff::PatchOpts::default();
    // `--decorate[=<style>]` / `--no-decorate`, shared with `git log`. `None` means
    // no flag was given, so `log.decorate` (and then git's `auto` default) decides.
    let mut cli_decorate: Option<DecorateStyle> = None;
    // `--decorate-refs=<pattern>` / `--decorate-refs-exclude=<pattern>` (both
    // repeatable) and `--clear-decorations`, which empties them again and drops
    // git's default known-namespace include list.
    let mut decorate_refs: Vec<String> = Vec::new();
    let mut decorate_refs_exclude: Vec<String> = Vec::new();
    let mut default_decoration_filter = true;
    // Set while a `--decorate-refs`/`--decorate-refs-exclude` given in the
    // separate-value form waits for its pattern in the next argv token.
    let mut pending_decorate_refs: Option<bool> = None;
    // `--source`: annotate each shown commit with the argument it was reached
    // from (`\t<source>` after the hash), as `git log --source` does.
    let mut source_mode = false;
    // `--use-mailmap`/`--mailmap`: `None` until a flag is seen, then `log.mailmap`
    // supplies the default.
    let mut cli_mailmap: Option<bool> = None;
    // `-L<range>:<file>`, repeatable: line-level history (see `line_log`). Shared
    // with `git log` — `cmd_show` runs the same `cmd_log_init_finish`.
    let mut line_ranges: Vec<String> = Vec::new();
    let mut pending_line_range = false;
    // `cmd_show` starts with `rev.no_walk = 1` and hands the pending list to
    // `cmd_log_walk` only when something cleared it — an UNINTERESTING object
    // (`^<rev>`, a range, `<rev>^!`) or an explicit `--do-walk`. A later
    // `--no-walk` sets it again, so the flag is positional on both sides.
    // `unsorted_input` never reaches `cmd_show`'s own loop, which is why
    // `--no-walk=sorted` and `--no-walk=unsorted` are indistinguishable here.
    let mut no_walk = true;
    // `--all`, `--branches`/`--tags`/`--remotes`, `--glob` and the `--exclude`
    // patterns they consume, slotted at the argument index they were written at
    // so their tips land where `setup_revisions()` pends them.
    let mut ref_selections: Vec<super::log::RefSelection> = Vec::new();
    let mut ref_excludes: Vec<String> = Vec::new();
    // Set while a detached `--glob <pattern>` (`Some(true)`) or
    // `--exclude <pattern>` (`Some(false)`) waits for its value.
    let mut pending_ref_value: Option<bool> = None;
    // `--reverse`: reverses `cmd_log_walk`'s output. Inert while `no_walk` holds,
    // because `cmd_show` prints its pending list without consulting it.
    let mut reverse = false;
    // `--topo-order` / `--date-order`, applied to the walk a cleared `no_walk`
    // hands to `cmd_log_walk`.
    let mut order = super::log::Order::Default;
    // `--expand-tabs[=<n>]` / `--no-expand-tabs`; `None` keeps the indented
    // formats on git's `expand_tabs_in_log_default` of 8.
    let mut expand_tabs: Option<usize> = None;

    for a in args {
        let s = a.as_str();
        // parse_options_step()'s `internal_help`. `git show` is `builtin/log.c`,
        // so the block it prints is `git log`'s — on stdout at 129.
        if !after_dashdash && s == "-h" {
            return Ok(super::show_usage(super::log::USAGE));
        }
        // `--help-all` is the same block with the hidden `--i-still-use-this`
        // left in (`USAGE_FULL`), and `git show` shares that too.
        if !after_dashdash && s == "--help-all" {
            return Ok(super::show_usage(super::log::USAGE_ALL));
        }
        // Everything after `--` is a pathspec, even tokens that look like flags:
        // `git show -- --stat` limits by the path `--stat`, it does not enable stat.
        if after_dashdash {
            pathspecs.push(a.as_bytes().to_vec());
            continue;
        }
        if let Some(kind) = pending_pickaxe.take() {
            match kind {
                'S' => pickaxe_s = Some(a.clone()),
                _ => pickaxe_g = Some(a.clone()),
            }
            continue;
        }
        // The value checks `diff_opt_parse`'s callbacks run as each option is seen.
        // `cmd_show` runs the same `cmd_log_init_finish` as `git log`, so a diff
        // option's value is validated here whether or not this command renders it.
        if let Some(line) = super::diff_optval::reject(s) {
            eprintln!("{line}");
            return Ok(ExitCode::from(129));
        }
        if std::mem::take(&mut pending_line_range) {
            line_ranges.push(a.clone());
            continue;
        }
        if let Some(include) = pending_decorate_refs.take() {
            if include {
                decorate_refs.push(a.clone());
            } else {
                decorate_refs_exclude.push(a.clone());
            }
            continue;
        }
        if let Some(is_glob) = pending_ref_value.take() {
            if is_glob {
                if negate_revs {
                    no_walk = false;
                }
                ref_selections.push(super::log::RefSelection::new(
                    specs.len(),
                    super::log::RefSelector::Glob,
                    Some(s),
                    std::mem::take(&mut ref_excludes),
                    negate_revs,
                ));
            } else {
                ref_excludes.push(a.clone());
            }
            continue;
        }
        match s {
            "-S" => pending_pickaxe = Some('S'),
            "-G" => pending_pickaxe = Some('G'),
            "-L" => pending_line_range = true,
            "--" => after_dashdash = true,
            "-p" | "-u" | "--patch" => formats.patch = true,
            // `-s` resets the diff output format rather than adding to it, which is
            // why `-s --name-only` and `--name-only -s` behave differently.
            "-s" | "--no-patch" => formats = Formats::only_no_output(),
            // `-q`/`--quiet` (position-independent, unlike `-s`): folded into
            // `no_output` after parsing so an explicit `-p`/`--stat` still wins.
            "-q" | "--quiet" => quiet = true,
            "--no-quiet" => quiet = false,
            "--name-only" => formats.name_only = true,
            "--name-status" => formats.name_status = true,
            "-z" => z = true,
            "--stdin" => read_stdin = true,
            "--not" => negate_revs = !negate_revs,
            "--numstat" => formats.numstat = true,
            "--shortstat" => formats.shortstat = true,
            "--summary" => formats.summary = true,
            "--raw" => formats.raw = true,
            // `diff_opt_parse`'s two combining spellings: each `OPT_BITOP` sets
            // its own format bit *and* `DIFF_FORMAT_PATCH`, so they are exactly
            // `-p --raw` and `-p --stat`.
            "--patch-with-raw" => {
                formats.patch = true;
                formats.raw = true;
            }
            "--patch-with-stat" => {
                formats.patch = true;
                formats.stat = true;
            }
            "--stat" => formats.stat = true,
            "--oneline" => {
                pretty = Pretty::Oneline;
                pretty_given = true;
                // git`s `--oneline` is `--pretty=oneline --abbrev-commit`.
                cli_abbrev = Some(true);
            }
            // `--notes[=<ref>]`/`--show-notes[=<ref>]`/`--no-notes`, as `git log`
            // takes them: a later flag overrides an earlier one.
            "--notes" | "--show-notes" => {
                notes_opt.enable_default();
                notes_opt.given = true;
            }
            "--no-notes" | "--no-show-notes" => {
                notes_opt.disable();
                notes_opt.given = true;
            }
            // `log.abbrevCommit`/`log.date`/`log.showRoot` overrides, mirroring
            // `git log`. There is no `--no-root`; `--root` only forces it on.
            // `--encoding=<enc>`: the encoding commit messages are re-coded into; this
            // port writes them as stored, which is `utf-8`/`none`.
            s if s.starts_with("--encoding=") => {
                let v = &s["--encoding=".len()..];
                if !super::blame::encoding_is_passthrough(v) {
                    bail!(
                        "unsupported option {s} (only utf-8 and none are ported; re-coding \
                         commit messages is not)"
                    );
                }
            }
            "--abbrev-commit" => cli_abbrev = Some(true),
            "--no-abbrev-commit" => cli_abbrev = Some(false),
            "--root" => force_root = true,
            // `--first-parent`: follow only the first parent in a walk, and show a
            // merge as a plain diff against its first parent instead of the dense
            // combined (`--cc`) diff. A no-op for a single non-merge commit.
            "--first-parent" => first_parent = true,
            // `sort_in_topological_order()` runs inside `prepare_revision_walk`,
            // which `cmd_show` only reaches once something cleared `no_walk` —
            // so these reorder a `git show <range>` and are inert otherwise.
            "--topo-order" => order = super::log::Order::Topo,
            "--date-order" => order = super::log::Order::Date,
            "--no-first-parent" => first_parent = false,
            // Ref decorations on the `commit <id>` / oneline header, exactly as
            // `git log` renders them (same filter, same ordering, same colors).
            "--decorate" => cli_decorate = Some(DecorateStyle::Short),
            "--no-decorate" => cli_decorate = Some(DecorateStyle::Off),
            "--decorate-refs" => pending_decorate_refs = Some(true),
            "--decorate-refs-exclude" => pending_decorate_refs = Some(false),
            // git's `clear_decorations_callback`: forget every pattern given so
            // far and stop applying the default namespace filter.
            "--clear-decorations" => {
                decorate_refs.clear();
                decorate_refs_exclude.clear();
                default_decoration_filter = false;
            }
            "--source" => source_mode = true,
            "--no-source" => source_mode = false,
            "--use-mailmap" | "--mailmap" => cli_mailmap = Some(true),
            "--no-use-mailmap" | "--no-mailmap" => cli_mailmap = Some(false),
            // We never colorize; accept the flags that request no/auto color.
            "--no-color" | "--color=never" | "--color=auto" => {}
            _ => {
                if let Some(v) = s.strip_prefix("--date=") {
                    match parse_date_mode(v) {
                        Some(m) => cli_date = Some(m),
                        None => return Ok(fatal(&format!("unknown date format {v}\n"))),
                    }
                } else if let Some(spec) = s
                    .strip_prefix("--format=")
                    .or_else(|| s.strip_prefix("--pretty="))
                {
                    // git validates each `--pretty`/`--format` occurrence eagerly,
                    // before resolving any revision, and rejects an invalid one
                    // wherever it appears with exit 128.
                    match parse_pretty(spec) {
                        Some(p) => {
                            pretty = p;
                            pretty_given = true;
                        }
                        None => return Ok(fatal(&format!("invalid --pretty format: {spec}\n"))),
                    }
                } else if let Some(v) = s
                    .strip_prefix("--notes=")
                    .or_else(|| s.strip_prefix("--show-notes="))
                {
                    notes_opt.enable_ref(v);
                    notes_opt.given = true;
                } else if let Some(v) = s.strip_prefix("--decorate=") {
                    // git's `decorate_callback` dies on a value its
                    // `parse_decoration_style` rejects, unlike `log.decorate`.
                    match super::log::parse_decoration_style(v) {
                        Some(st) => cli_decorate = Some(st),
                        None => return Ok(fatal(&format!("invalid --decorate option: {v}\n"))),
                    }
                } else if let Some(v) = s.strip_prefix("--decorate-refs=") {
                    decorate_refs.push(v.to_string());
                } else if let Some(v) = s.strip_prefix("--decorate-refs-exclude=") {
                    decorate_refs_exclude.push(v.to_string());
                } else if let Some(v) = s.strip_prefix("-S") {
                    pickaxe_s = Some(v.to_string());
                } else if let Some(v) = s.strip_prefix("-G") {
                    pickaxe_g = Some(v.to_string());
                } else if let Some(v) = s.strip_prefix("-L") {
                    line_ranges.push(v.to_string());
                } else if let Some(v) = s.strip_prefix("--stat=") {
                    // `--stat[=<width>[,<name-width>[,<count>]]]`: an aliased form that
                    // sets the total width (and optionally the name column / line cap),
                    // and, like every `--stat*` flag, requests the diffstat.
                    formats.stat = true;
                    parse_stat_geometry(&mut stat_widths, v);
                } else if let Some(v) = s.strip_prefix("--stat-width=") {
                    formats.stat = true;
                    stat_widths.width = parse_stat_i64(v);
                } else if let Some(v) = s.strip_prefix("--stat-name-width=") {
                    formats.stat = true;
                    stat_widths.name_width = parse_stat_i64(v);
                } else if let Some(v) = s.strip_prefix("--stat-graph-width=") {
                    formats.stat = true;
                    stat_widths.graph_width = parse_stat_i64(v);
                } else if let Some(v) = s.strip_prefix("--stat-count=") {
                    formats.stat = true;
                    stat_widths.count = parse_stat_i64(v);
                // Rename detection: on by default for a porcelain, and these are the
                // knobs `diff_opt_parse()` gives to turn it off or retune it.
                // The patch-shaping options `diff_opt_parse()` takes.
                } else if s == "-w" || s == "--ignore-all-space" {
                    patch_opts.ws = super::diff::Whitespace::IgnoreAll;
                } else if s == "-b" || s == "--ignore-space-change" {
                    patch_opts.ws = super::diff::Whitespace::IgnoreChange;
                } else if s == "--ignore-space-at-eol" {
                    patch_opts.ws = super::diff::Whitespace::IgnoreAtEol;
                } else if s == "--full-index" {
                    patch_opts.full_index = true;
                } else if s == "-a" || s == "--text" {
                    patch_opts.text = true;
                } else if s == "-W" || s == "--function-context" {
                    patch_opts.func_context = true;
                } else if s == "--no-function-context" {
                    patch_opts.func_context = false;
                } else if s == "--no-prefix" {
                    patch_opts.src_prefix.clear();
                    patch_opts.dst_prefix.clear();
                } else if s == "--default-prefix" {
                    patch_opts.src_prefix = b"a/".to_vec();
                    patch_opts.dst_prefix = b"b/".to_vec();
                } else if let Some(v) = s.strip_prefix("--src-prefix=") {
                    patch_opts.src_prefix = v.as_bytes().to_vec();
                } else if let Some(v) = s.strip_prefix("--dst-prefix=") {
                    patch_opts.dst_prefix = v.as_bytes().to_vec();
                } else if let Some(v) = s
                    .strip_prefix("-U")
                    .filter(|v| !v.is_empty())
                    .or_else(|| s.strip_prefix("--unified="))
                {
                    match v.parse::<u32>() {
                        Ok(n) => patch_opts.ctx = n,
                        Err(_) => crate::git_fatal!("invalid argument to -U: {v}"),
                    }
                } else if s == "--no-renames" {
                    patch_opts.renames = Some(0);
                // `diff_opt_find_copies()`: a second `-C` is `--find-copies-harder`.
                } else if s == "-C" || s == "--find-copies" {
                    patch_opts.rename_score = 0;
                    if patch_opts.renames == Some(super::diffcore_rename::DETECT_COPY) {
                        patch_opts.find_copies_harder = true;
                    } else {
                        patch_opts.renames = Some(super::diffcore_rename::DETECT_COPY);
                    }
                } else if let Some(v) = s
                    .strip_prefix("--find-copies=")
                    .or_else(|| s.strip_prefix("-C").filter(|r| !r.is_empty()))
                {
                    let (score, rest) = super::diffcore_rename::parse_rename_score(v);
                    if !rest.is_empty() {
                        crate::git_fatal!("invalid argument to -C: {v}");
                    }
                    patch_opts.rename_score = score;
                    if patch_opts.renames == Some(super::diffcore_rename::DETECT_COPY) {
                        patch_opts.find_copies_harder = true;
                    } else {
                        patch_opts.renames = Some(super::diffcore_rename::DETECT_COPY);
                    }
                } else if s == "--find-copies-harder" {
                    patch_opts.find_copies_harder = true;
                } else if s == "--no-find-copies-harder" {
                    patch_opts.find_copies_harder = false;
                // `diff_opt_break_rewrites()`: `-B[<n>][/<m>]`, packed as `n | (m << 16)`.
                } else if s == "-B" || s == "--break-rewrites" {
                    patch_opts.break_opt = 0;
                } else if let Some(v) = s
                    .strip_prefix("--break-rewrites=")
                    .or_else(|| s.strip_prefix("-B").filter(|r| !r.is_empty()))
                {
                    match super::diffcore_rename::parse_break_opt(v) {
                        Ok(n) => patch_opts.break_opt = n,
                        Err(()) => crate::git_fatal!("invalid argument to -B: {v}"),
                    }
                } else if s == "--renames" || s == "-M" || s == "--find-renames" {
                    patch_opts.renames = Some(super::diffcore_rename::DETECT_RENAME);
                } else if let Some(v) = s
                    .strip_prefix("-M")
                    .or_else(|| s.strip_prefix("--find-renames="))
                {
                    patch_opts.renames = Some(super::diffcore_rename::DETECT_RENAME);
                    let (score, rest) = super::diffcore_rename::parse_rename_score(v);
                    if !rest.is_empty() {
                        crate::git_fatal!("invalid argument to -M: {v}");
                    }
                    patch_opts.rename_score = score;
                // `--abbrev[=<n>]` / `--no-abbrev`: the width of every abbreviated id
                // in the run — `%h`, the oneline id, the `--raw` columns and the patch
                // `index` line. Applied as a `core.abbrev` override once the repo is
                // open, so one setting reaches all of them.
                } else if s == "--abbrev" {
                    abbrev_len = Some(DEFAULT_ABBREV);
                    abbrev_raw = None;
                    no_abbrev = false;
                } else if s == "--no-abbrev" {
                    no_abbrev = true;
                    abbrev_len = None;
                    abbrev_raw = None;
                } else if let Some(v) = s.strip_prefix("--abbrev=") {
                    // The clamp needs the hash width, so the raw request is kept
                    // here and read once the repo is open.
                    abbrev_raw = Some(v.to_string());
                    abbrev_len = None;
                    no_abbrev = false;
                // `--no-walk[=(sorted|unsorted)]` / `--do-walk`. `cmd_show` never
                // reads `unsorted_input`, so the two values only have to be
                // validated, not acted on.
                // `revs->expand_tabs_in_log`: how wide a tab is when the message
                // is reprinted under a four-space indent. A bare `--expand-tabs`
                // is `expand_tabs_in_log_default` (8), `--no-expand-tabs` is zero.
                } else if s == "--expand-tabs" {
                    expand_tabs = Some(8);
                } else if s == "--no-expand-tabs" {
                    expand_tabs = Some(0);
                } else if let Some(v) = s.strip_prefix("--expand-tabs=") {
                    match v.parse::<usize>() {
                        Ok(n) => expand_tabs = Some(n),
                        Err(_) => {
                            eprintln!("fatal: '{v}': not a non-negative integer");
                            return Ok(ExitCode::from(128));
                        }
                    }
                } else if s == "--no-walk" {
                    no_walk = true;
                } else if let Some(v) = s.strip_prefix("--no-walk=") {
                    if v != "sorted" && v != "unsorted" {
                        eprintln!("error: invalid argument to --no-walk");
                        eprintln!("fatal: unrecognized argument: {s}");
                        return Ok(ExitCode::from(128));
                    }
                    no_walk = true;
                } else if s == "--do-walk" {
                    no_walk = false;
                // `--reverse` reverses what `cmd_log_walk` emits; `cmd_show`'s own
                // pending loop never consults it, so it does nothing while
                // `no_walk` stands.
                } else if s == "--reverse" {
                    // `revs->reverse ^= 1` (revision.c): a toggle, so an even
                    // number of `--reverse`s leaves the order alone.
                    reverse = !reverse;
                // The ref-selecting pseudo-options, at the slot `setup_revisions()`
                // would pend their refs at. `--glob` and `--exclude` may carry
                // their value as the next argv element (`parse_long_opt`), which
                // the pending flags above collect.
                } else if s == "--glob" {
                    pending_ref_value = Some(true);
                } else if s == "--exclude" {
                    pending_ref_value = Some(false);
                } else if let Some(v) = s.strip_prefix("--exclude=") {
                    ref_excludes.push(v.to_string());
                } else if let Some((sel, pattern)) = super::log::ref_selector(s) {
                    if negate_revs {
                        no_walk = false;
                    }
                    ref_selections.push(super::log::RefSelection::new(
                        specs.len(),
                        sel,
                        pattern,
                        std::mem::take(&mut ref_excludes),
                        negate_revs,
                    ));
                } else if s.starts_with('-') {
                    bail!("unsupported option {s}");
                } else {
                    // `add_pending_object_with_path()` clears `revs->no_walk` the
                    // moment an UNINTERESTING object is pended, so the two are
                    // positional against each other: `git show A..B --no-walk`
                    // prints the pending objects, `git show --no-walk A..B` walks.
                    // `handle_revision_arg_1()` refuses a bare `..` before
                    // `handle_dotdot()` ever sees it, so it is prune data rather
                    // than `HEAD..HEAD`; the pathspec layer then rejects it for
                    // leaving the repository. See
                    // [`crate::objname::is_parent_directory_pathspec`].
                    if crate::objname::is_parent_directory_pathspec(s, seen_dashdash) {
                        pathspecs.push(s.as_bytes().to_vec());
                        continue;
                    }
                    if super::log::argument_excludes(s, negate_revs) {
                        no_walk = false;
                    }
                    specs.push(s);
                    spec_negated.push(negate_revs);
                }
            }
        }
    }
    // `-q`/`--quiet` sets git's NO_OUTPUT bit low-priority: `git show -q` suppresses
    // the default patch, but `-q -p`/`-q --stat` still render because the explicit
    // format bit wins in `Formats::resolve`. `--name-only -q` (NO_OUTPUT + NAME with
    // no third format) is rejected there, matching git's `diff_setup_done`.
    if quiet {
        formats.no_output = true;
    }
    // `-L` (`rev->line_level_traverse`), rejected against the formats and the
    // pathspec exactly as `cmd_log_init_finish` rejects them.
    let line_level = !line_ranges.is_empty();
    if line_level {
        if formats.stat {
            return Ok(fatal("-L does not yet support the requested diff format\n"));
        }
        if !pathspecs.is_empty() {
            return Ok(fatal("-L<range>:<file> cannot be used with pathspec\n"));
        }
    }
    if pending_line_range {
        eprintln!("error: switch `L' requires a value");
        return Ok(ExitCode::from(129));
    }
    // A trailing `--decorate-refs`/`--decorate-refs-exclude` with nothing after it
    // is git's parse-options "requires a value" usage error (exit 129).
    if let Some(include) = pending_decorate_refs {
        let name = if include {
            "decorate-refs"
        } else {
            "decorate-refs-exclude"
        };
        eprintln!("error: option `{name}' requires a value");
        return Ok(ExitCode::from(129));
    }
    if let Pretty::User(fmt) = &pretty {
        // Reject unknown placeholders before any output is produced.
        check_format(fmt)?;
    }
    stdin_text = if read_stdin {
        use std::io::Read as _;
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else {
        String::new()
    };
    // `--stdin` lines are read after the command line is scanned, so they take the
    // `--not` state the last argument left behind — git reads them through the same
    // `handle_revision_arg()` with the same `flags`. They are read inside
    // `read_revisions_from_stdin()` though, which clears
    // `warn_on_object_refname_ambiguity` for its whole loop, so the boundary
    // between argv and stdin specs is remembered for the warning below.
    let argv_specs = specs.len();
    for line in stdin_text.lines().filter(|l| !l.is_empty()) {
        if super::log::argument_excludes(line, negate_revs) {
            no_walk = false;
        }
        specs.push(line);
        spec_negated.push(negate_revs);
    }

    // `revs->def`: the fallback pending object is added with no flags at all, so a
    // trailing `--not` never turns the implicit `HEAD` into an exclusion. A ref
    // selection is `rev_input_given` too (`init_all_refs_cb`), so `git show --tags`
    // never adds `HEAD` on top of the tags it named.
    if specs.is_empty() && ref_selections.is_empty() {
        specs.push("HEAD");
        spec_negated.push(false);
    }

    let mut repo = gix::discover(".")?;
    let hex_len = repo.object_hash().len_in_hex();

    // `revs->abbrev` reaches every abbreviation in the run, so it goes in front of
    // the same `core.abbrev` lookup gitoxide already makes. `--no-abbrev` is the one
    // exception: it prints whole ids but leaves the `index` line on the configured
    // default, which is pinned here before the override lands.
    if let Some(v) = &abbrev_raw {
        abbrev_len = Some(crate::abbrev::parse_abbrev_arg(v, hex_len));
    }
    if no_abbrev {
        patch_opts.index_abbrev = Some(crate::abbrev::configured_abbrev(&repo, hex_len));
        abbrev_len = Some(hex_len);
    }
    if let Some(n) = abbrev_len {
        let mut config = repo.config_snapshot_mut();
        config.append_config(
            Some(format!("core.abbrev={}", n.clamp(MINIMUM_ABBREV, hex_len))),
            gix::config::Source::Cli,
        )?;
        config.commit()?;
    }

    // Config supplies the defaults for the display knobs `git show` shares with
    // `git log`; the CLI flags parsed above win where present. git reads these in
    // `git_log_config` and validates `log.date` there — an invalid value is fatal
    // even when `--date` later overrides it, so it is checked unconditionally.
    let (abbrev_commit, date_mode, show_root, decorate, use_mailmap) = {
        let snap = repo.config_snapshot();
        let cfg_abbrev = snap.boolean("log.abbrevCommit").unwrap_or(false);
        // `log.decorate` supplies the default decoration style; git's built-in
        // default is `auto`, which is `short` on a terminal and off in a pipe. An
        // invalid config value is `Off` (git's `decoration_style = 0`), never fatal.
        let cfg_decorate: DecorateStyle = match snap.boolean("log.decorate") {
            Some(true) => DecorateStyle::Short,
            Some(false) => DecorateStyle::Off,
            None => match snap.string("log.decorate") {
                Some(v) => super::log::parse_decoration_style(&v.to_str_lossy())
                    .unwrap_or(DecorateStyle::Off),
                None if std::io::stdout().is_terminal() => DecorateStyle::Short,
                None => DecorateStyle::Off,
            },
        };
        // `log.mailmap` has defaulted to true since git 2.24.
        let cfg_mailmap = snap.boolean("log.mailmap").unwrap_or(true);
        // `log.showRoot` defaults to true: a root commit is shown as a creation
        // event (its diff against the empty tree). `--root` forces it on; there is
        // no `--no-root`, so config is the only way to suppress the root diff.
        let cfg_show_root = snap.boolean("log.showRoot").unwrap_or(true);
        let cfg_date = match snap.string("log.date") {
            Some(v) => {
                let v = v.to_str_lossy();
                match parse_date_mode(&v) {
                    Some(m) => m,
                    None => return Ok(fatal(&format!("unknown date format {v}\n"))),
                }
            }
            None => DateMode::Default,
        };
        // `diff.statNameWidth`/`diff.statGraphWidth` seed the `--stat` name/graph
        // column caps. A `--stat*` flag already wrote a concrete value (possibly `0`);
        // only a slot still at the `-1` sentinel falls back to config, so a flag always
        // wins (`git_diff_ui_config()` runs before `diff_opt_stat()`).
        if stat_widths.name_width == -1 {
            if let Some(n) = snap.integer("diff.statNameWidth") {
                if n > 0 {
                    stat_widths.name_width = n;
                }
            }
        }
        if stat_widths.graph_width == -1 {
            if let Some(n) = snap.integer("diff.statGraphWidth") {
                if n > 0 {
                    stat_widths.graph_width = n;
                }
            }
        }
        (
            cli_abbrev.unwrap_or(cfg_abbrev),
            cli_date.unwrap_or(cfg_date),
            force_root || cfg_show_root,
            cli_decorate.unwrap_or(cfg_decorate),
            cli_mailmap.unwrap_or(cfg_mailmap),
        )
    };

    // The commit→refs map for `--decorate`, filtered exactly as `git log` filters
    // it; skipped entirely when no decorations will be shown so the ref scan costs
    // nothing on a plain `git show`.
    let decorations = if decorate == DecorateStyle::Off {
        None
    } else {
        let filter = super::log::DecorationFilter::build(
            &repo,
            &decorate_refs,
            &decorate_refs_exclude,
            default_decoration_filter,
        );
        Some(super::log::build_decorations(&repo, &filter)?)
    };
    // `--use-mailmap` / `log.mailmap`: loaded once and shared by every commit.
    let mailmap = use_mailmap.then(|| Mailmap::load(&repo));

    // git resolves every revision before rendering anything, so a bad revision
    // produces no stdout at all even when an earlier one was fine. Ranges (`a..b`),
    // symmetric differences (`a...b`), and exclusions (`^a`) turn the request into
    // a revision walk; plain object names are shown directly.
    let mut walk_tips: Vec<ObjectId> = Vec::new();
    let mut walk_hidden: Vec<ObjectId> = Vec::new();
    let mut plain: Vec<(String, ObjectId)> = Vec::new();
    // Commits the command line already caused to be parsed, which is as far as
    // `mark_parents_uninteresting()` reaches while `no_walk` stands.
    let mut parsed_commits: std::collections::HashSet<ObjectId> =
        std::collections::HashSet::new();
    // Parallel to `walk_tips` and used only under `--source`: the name git records
    // for each pending object, which is the endpoint token rather than the whole
    // argument (`main~2..main` names its tip `main`).
    let mut walk_tip_sources: Vec<String> = Vec::new();
    // The ref-selecting pseudo-options pend their refs at the argument index they
    // were written at, ahead of the revision that stood there.
    let mut push_ref_tips = |at: usize,
                             walk_tips: &mut Vec<ObjectId>,
                             walk_tip_sources: &mut Vec<String>,
                             walk_hidden: &mut Vec<ObjectId>,
                             plain: &mut Vec<(String, ObjectId)>,
                             no_walk: &mut bool|
     -> Result<()> {
        for sel in ref_selections.iter().filter(|s| s.at == at) {
            let mut pend = |oid: ObjectId, name: &str| {
                if sel.negated {
                    *no_walk = false;
                    walk_hidden.push(oid);
                    return;
                }
                plain.push((name.to_string(), oid));
                walk_tips.push(oid);
                walk_tip_sources.push(name.to_string());
            };
            for reference in repo.references()?.all()? {
                let Ok(reference) = reference else { continue };
                let full = reference.name().as_bstr().to_string();
                let Some(name) = sel.selects(&full) else { continue };
                let Ok(id) = reference.into_fully_peeled_id() else { continue };
                let oid = id.detach();
                if !repo.find_object(oid).is_ok_and(|o| o.kind == Kind::Commit) {
                    continue;
                }
                pend(oid, name);
            }
            if sel.head && !sel.excluded("HEAD") {
                if let Some(id) = repo.head().ok().and_then(|mut h| h.try_peel_to_id().ok().flatten())
                {
                    pend(id.detach(), "HEAD");
                }
            }
        }
        Ok(())
    };
    for (at, (spec, negated)) in specs.iter().zip(spec_negated.iter().copied()).enumerate() {
        push_ref_tips(
            at,
            &mut walk_tips,
            &mut walk_tip_sources,
            &mut walk_hidden,
            &mut plain,
            &mut no_walk,
        )?;
        // `handle_revision_arg_1()` puts every endpoint of the token through
        // `get_oid_with_context()`, so `get_oid_basic()`'s ambiguity warning fires
        // once per endpoint, and not at all for what `--stdin` supplied.
        if at < argv_specs {
            for endpoint in super::log::revision_endpoints(spec) {
                crate::objname::warn_ambiguous_refname(&repo, endpoint);
            }
        }
        // The `~<n>`/`^<n>` chains a token navigates are how far
        // `mark_parents_uninteresting()` can reach while `no_walk` stands.
        for endpoint in spec.trim_start_matches('^').split("..") {
            let e = endpoint.trim_start_matches('.');
            parsed_commits.extend(super::log::navigation_path(
                &repo,
                if e.is_empty() { "HEAD" } else { e },
            ));
        }
        // `handle_dotdot()` is the first thing `handle_revision_arg_1()` tries,
        // so a range `handle_dotdot_1()` rejects dies here — before the token is
        // read as anything else. gitoxide's `rev_parse()` peels a `<a>...<b>` on
        // its own and so *succeeds* where git dies, which is why the question has
        // to be asked ahead of it rather than in the `Err` arm below: without
        // this, `git show <tag-of-a-tree>...HEAD` printed a commit and exited 0
        // where git prints `error: object … is a tree, not a commit` and
        // `fatal: Invalid symmetric difference expression …` at 128.
        if let Some(message) = crate::objname::dotdot_fatal(&repo, spec) {
            eprint!("{message}");
            return Ok(ExitCode::from(128));
        }
        let parsed = match repo.rev_parse(BStr::new(*spec)) {
            Ok(p) => p.detach(),
            Err(_) => return Ok(bad_revision(&repo, spec)),
        };
        match parsed {
            // `--not <rev>` and `^<rev>` are the same thing twice: `handle_revision_arg_1`
            // flips `UNINTERESTING` once for the `^` and `setup_revisions` flips it once
            // for the `--not`, so the two together cancel back to a positive revision.
            RevSpec::Include(id) if negated => {
                plain.push(((*spec).to_string(), id));
                walk_hidden.push(id);
            }
            RevSpec::Include(id) => {
                plain.push(((*spec).to_string(), id));
                walk_tips.push(id);
                walk_tip_sources.push((*spec).to_string());
            }
            // `<rev>^!` is `add_parents_only()` with the flags flipped: every parent
            // is pended UNINTERESTING *first*, and only then does
            // `handle_revision_arg_1()` fall through and pend the commit itself
            // positive. That clears `no_walk` and makes the record a one-commit
            // walk. It matters as soon as a second argument names one of those
            // parents — `git show HEAD^! HEAD^@` prints the merge alone, because
            // the `^@` parents are already excluded by the `^!`.
            RevSpec::ExcludeParents(id) => {
                let parents = parents_of(&repo, id)?;
                if negated {
                    for p in parents {
                        plain.push(((*spec).to_string(), p));
                        walk_tips.push(p);
                        walk_tip_sources.push((*spec).to_string());
                    }
                    plain.push(((*spec).to_string(), id));
                    walk_hidden.push(id);
                } else {
                    for p in &parents {
                        plain.push(((*spec).to_string(), *p));
                    }
                    walk_hidden.extend(parents);
                    plain.push(((*spec).to_string(), id));
                    walk_tips.push(id);
                    walk_tip_sources.push((*spec).to_string());
                }
            }
            RevSpec::Exclude(id) if negated => {
                plain.push(((*spec).to_string(), id));
                walk_tips.push(id);
                walk_tip_sources.push((*spec).to_string());
            }
            RevSpec::Exclude(id) => {
                plain.push(((*spec).to_string(), id));
                walk_hidden.push(id);
            }
            RevSpec::Range { from, to } => {
                // `A..B` is `^A B`, and under `--not` each endpoint takes the other
                // side — `handle_dotdot_1` derives both from the one `flags` word,
                // and pends the excluded endpoint first.
                let (tip, hidden, right) = if negated { (from, to, false) } else { (to, from, true) };
                let name = range_endpoint(spec, "..", right);
                plain.push((range_endpoint(spec, "..", !right), hidden));
                walk_hidden.push(hidden);
                // The positive endpoint is an ordinary pending object, which is
                // what `cmd_show` prints when a later `--no-walk` restored the
                // flag the range had cleared.
                plain.push((name.clone(), tip));
                walk_tips.push(tip);
                walk_tip_sources.push(name);
            }
            RevSpec::Merge { theirs, ours } => {
                // `theirs...ours` = reachable from either but not both, which git
                // computes as `theirs ours --not $(merge-base theirs ours)`. Under
                // `--not` the endpoints take the excluded flags and the merge bases
                // the positive ones: the same three objects with their sides swapped.
                let bases: Vec<ObjectId> = repo
                    .merge_bases_many(theirs, &[ours])?
                    .iter()
                    .map(|c| c.detach())
                    .collect();
                // `paint_down_to_common()` parses its way from both endpoints
                // past the bases, so a base's whole ancestry is loaded before
                // `mark_parents_uninteresting()` runs over it.
                parsed_commits.extend(super::log::ancestor_closure(&repo, &bases)?);
                // `handle_dotdot_1()` pends the merge bases before either endpoint.
                let (left, right) =
                    (range_endpoint(spec, "...", false), range_endpoint(spec, "...", true));
                for mb in &bases {
                    plain.push(((*spec).to_string(), *mb));
                }
                if negated {
                    for mb in bases {
                        walk_tips.push(mb);
                        walk_tip_sources.push((*spec).to_string());
                    }
                    plain.push((left, theirs));
                    plain.push((right, ours));
                    walk_hidden.push(theirs);
                    walk_hidden.push(ours);
                } else {
                    walk_hidden.extend(bases);
                    plain.push((left.clone(), theirs));
                    plain.push((right.clone(), ours));
                    walk_tips.push(theirs);
                    walk_tip_sources.push(left);
                    walk_tips.push(ours);
                    walk_tip_sources.push(right);
                }
            }
            // `<rev>^@` is `add_parents_only()` with the plain flags: the parents enter
            // the pending list positive, and nothing about them is UNINTERESTING, so
            // `no_walk` survives and `git show HEAD^@` prints the parents themselves
            // rather than walking their history. `--not` is what makes them exclusions.
            RevSpec::IncludeOnlyParents(id) => {
                for p in parents_of(&repo, id)? {
                    if negated {
                        plain.push(((*spec).to_string(), p));
                        walk_hidden.push(p);
                    } else {
                        plain.push(((*spec).to_string(), p));
                        walk_tips.push(p);
                        walk_tip_sources.push((*spec).to_string());
                    }
                }
            }
        }
    }
    // Ref selections written after the last revision argument.
    push_ref_tips(
        specs.len(),
        &mut walk_tips,
        &mut walk_tip_sources,
        &mut walk_hidden,
        &mut plain,
        &mut no_walk,
    )?;
    // Any `^<rev>` clears `revs->no_walk` (`add_pending_object_with_path`), so `cmd_show` hands
    // the whole pending list to `cmd_log_walk` instead of printing the objects one by one. Both
    // gates that list then passes — `check_single_commit`'s `deref_tag` and `handle_commit`'s tag
    // loop — see through an annotated tag to the commit it names, on the positive and the negative
    // side alike, which is why `git show v1 ^v1` prints nothing rather than the tag's own header.
    let needs_walk = !no_walk;
    if needs_walk {
        for id in walk_tips.iter_mut().chain(walk_hidden.iter_mut()) {
            if let Some(peeled) = repo
                .find_object(*id)
                .ok()
                .and_then(|o| o.peel_tags_to_end().ok())
            {
                *id = peeled.id;
            }
        }
    }
    // What is left after that peel and is still not a commit — a tree, a blob — is an
    // object for `--objects` to list, never a commit `handle_commit()` hands back to
    // the walk, so `git show ^HEAD HEAD^{tree}` walks from nothing and prints nothing.
    // `-L` is excluded because its own gate runs first: `check_single_commit()` dies
    // naming the non-commit rather than quietly dropping it.
    if needs_walk && !line_level {
        let mut tips = Vec::with_capacity(walk_tips.len());
        let mut sources = Vec::with_capacity(walk_tip_sources.len());
        for (tip, source) in walk_tips.drain(..).zip(walk_tip_sources.drain(..)) {
            if repo.find_object(tip).is_ok_and(|o| o.kind == Kind::Commit) {
                tips.push(tip);
                sources.push(source);
            }
        }
        walk_tips = tips;
        walk_tip_sources = sources;
    }
    // `cmd_show` hands its pending list to `cmd_log_walk`, so the traversal is
    // `git log`'s: a commit-date-ordered frontier whose ties break by the order
    // tips and parents entered it. Sharing `git log`'s walk is what keeps
    // `git show <range>` from ordering a merge's two lanes differently from
    // `git log <range>` — gitoxide's default `Sorting::BreadthFirst` did.
    let walked = if needs_walk && !line_level {
        let hidden = if walk_hidden.is_empty() {
            std::collections::HashSet::new()
        } else {
            super::log::ancestor_closure(&repo, &walk_hidden)?
        };
        let nodes = super::log::walk(
            &repo,
            &walk_tips,
            &walk_tip_sources,
            first_parent,
            &hidden,
            None,
            None,
        )?;
        if order == super::log::Order::Default {
            nodes
        } else {
            super::log::topo_sort(nodes, order == super::log::Order::Date)
        }
    } else {
        Vec::new()
    };
    // While `no_walk` stands nothing paints UNINTERESTING over the history, so a
    // pending object is dropped only when `mark_parents_uninteresting()` already
    // reached it (`get_commit_action`'s `commit_ignore`).
    let no_walk_hidden = if !needs_walk && !walk_hidden.is_empty() {
        super::log::no_walk_uninteresting(&repo, &walk_hidden, &parsed_commits)
    } else {
        std::collections::HashSet::new()
    };

    let selection = match formats.resolve() {
        Ok(sel) => sel,
        Err(FormatConflict) => {
            return Ok(fatal(
                "options '--name-only', '--name-status', '--check', and '-s' cannot be used together\n",
            ))
        }
    };

    // Compile `-G` once, in git's default (basic-regex) dialect, matching `git log`.
    let pickaxe = Pickaxe {
        s: pickaxe_s,
        g: match &pickaxe_g {
            Some(p) => Some(crate::revfilter::build_regex(
                p,
                crate::revfilter::Dialect::Basic,
                false,
            )?),
            None => None,
        },
    };

    let mut out: Vec<u8> = Vec::new();
    // git marks each commit it prints as SHOWN, so a commit named twice (or reached
    // twice by a walk) is printed once. Blobs, trees, and tags are not deduplicated.
    let mut shown: Vec<ObjectId> = Vec::new();
    // git's `rev_info.shown_one`, which drives the inter-record separator.
    let mut shown_one = false;
    if !notes_opt.given && (!pretty_given || matches!(pretty, Pretty::User(_))) {
        notes_opt.enable_default();
    }
    let notes_trees = super::notes::load_display(&repo, &notes_opt)?;
    let disp = DisplayOpts {
        notes: &notes_trees,
        abbrev_commit,
        date_mode,
        show_root,
        first_parent,
        stat: stat_widths,
        patch: patch_opts.clone(),
        decorate,
        decorations: decorations.as_ref(),
        mailmap: mailmap.as_ref(),
        z,
        expand_tabs,
    };
    if line_level {
        // `check_single_commit`: the ranges are resolved against exactly one pending
        // commit, so several positive endpoints leave the starting blob undefined.
        if walk_tips.len() > 1 {
            return Ok(fatal(&format!(
                "More than one commit to dig from: {} and {}?\n",
                walk_tip_sources.get(1).map(String::as_str).unwrap_or_default(),
                walk_tip_sources.first().map(String::as_str).unwrap_or_default()
            )));
        }
        let Some(start) = walk_tips.first().copied() else {
            return Ok(fatal("No commit specified?\n"));
        };
        let start = match repo.find_object(start).map_err(|e| e.to_string()).and_then(|o| o.peel_to_kind(Kind::Commit).map_err(|e| e.to_string())) {
            Ok(o) => o.id,
            Err(_) => {
                let name = walk_tip_sources.first().map(String::as_str).unwrap_or_default();
                return Ok(fatal(&format!("Non commit {name}?\n")));
            }
        };
        let tracked = match line_log::parse_lines(&repo, start, &line_ranges) {
            Ok(t) => t,
            Err(e) => return Ok(fatal(&format!("{}\n", e.0))),
        };
        if needs_walk {
            // A range endpoint clears `no_walk`, so this is `cmd_log_walk` — the same
            // line-level traversal `git log -L` runs, topological order included.
            let hidden = if walk_hidden.is_empty() {
                std::collections::HashSet::new()
            } else {
                super::log::ancestor_closure(&repo, &walk_hidden)?
            };
            let nodes = super::log::walk(&repo, &[start], &[], first_parent, &hidden, None, None)?;
            let nodes = super::log::topo_sort(nodes, false);
            let mut tracker = line_log::Tracker::new(&repo, start, tracked, first_parent);
            for node in &nodes {
                let (Some(range), _) = tracker.process(node.id, &node.parents)? else {
                    continue;
                };
                let pairs = line_log::queue_pairs(&range);
                show_one(&repo, &mut out, &node.id.to_string(), node.id, &pretty, selection, &pathspecs, &disp, &pickaxe, &mut shown, None, &mut shown_one, Some(&pairs))?;
            }
        } else {
            // `no_walk` never parses the pending commit's parents, so their tree ids
            // read back as absent and every tracked file is queued as a creation —
            // which is why `git show -L` prints the range as a brand-new file. A
            // merge takes the multi-parent path instead, whose bookkeeping clears the
            // commit's own record, so it shows a header and no diff at all.
            let commit = repo.find_object(start)?.try_into_commit()?;
            let is_merge = commit.parent_ids().count() > 1;
            let mut pairs: Vec<(line_log::Pair, Vec<line_log::Range>)> = Vec::new();
            if !is_merge {
                let mut tracker = line_log::Tracker::new(&repo, start, tracked, first_parent);
                if let (Some(range), _) = tracker.process(start, &[])? {
                    pairs = line_log::queue_pairs(&range);
                }
            }
            let spec = walk_tip_sources.first().map(String::as_str).unwrap_or("HEAD");
            show_one(&repo, &mut out, spec, start, &pretty, selection, &pathspecs, &disp, &pickaxe, &mut shown, source_mode.then_some(spec), &mut shown_one, Some(&pairs))?;
        }
    } else if needs_walk {
        // `--reverse` is applied to what the walk produced, which is where
        // `get_revision()` applies it too.
        let mut nodes = walked;
        if reverse {
            nodes.reverse();
        }
        for node in &nodes {
            let id = node.id;
            show_one(&repo, &mut out, &id.to_string(), id, &pretty, selection, &pathspecs, &disp, &pickaxe, &mut shown, source_mode.then_some(node.source.as_str()), &mut shown_one, None)?;
        }
    } else {
        // `cmd_show` reuses one `rev_info` across its pending loop, and the first
        // commit it hands to `cmd_log_walk` consumes `--reverse`
        // (`revs->reverse = 0; revs->reverse_output_stage = 1`, revision.c:4683).
        // Every commit after that is popped straight off `revs->commits`
        // (revision.c:4687-4692), past the `commit_ignore` check
        // `get_revision_internal()` would have applied — which is why
        // `git show --no-walk --reverse main ^main~2` prints `main~2` as well.
        let mut reverse_stage = false;
        // `handle_commit()` sets SEEN on the object, and the flag outlives the
        // one-entry walk it was set in, so a commit pended twice — the merge base
        // of `A...B` and the endpoint that names it, say — is walked once.
        let mut seen_pending: Vec<ObjectId> = Vec::new();
        for (spec, id) in &plain {
            let is_commit = repo.find_object(*id).is_ok_and(|o| o.kind == Kind::Commit);
            if is_commit {
                if seen_pending.contains(id) {
                    continue;
                }
                seen_pending.push(*id);
                if !reverse_stage && no_walk_hidden.contains(id) {
                    reverse_stage = reverse;
                    continue;
                }
                reverse_stage = reverse;
            }
            show_one(&repo, &mut out, spec, *id, &pretty, selection, &pathspecs, &disp, &pickaxe, &mut shown, source_mode.then_some(spec.as_str()), &mut shown_one, None)?;
        }
    }

    let mut stdout = std::io::stdout().lock();
    match stdout.write_all(&out).and_then(|()| stdout.flush()) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        // A downstream `| head` closing the pipe is not an error; git leaves by
        // way of SIGPIPE rather than returning a status of its own.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
            crate::sigpipe::exit_broken_pipe()
        }
        Err(e) => Err(e.into()),
    }
}

/// The parents of the commit `id` names, for the `<rev>^@` and `<rev>^!` forms.
///
/// `add_parents_only()` peels the tag chain before it reads the parent list, so
/// `v1^@` names the parents of the commit `v1` tags rather than failing on the tag
/// object itself.
fn parents_of(repo: &gix::Repository, id: ObjectId) -> Result<Vec<ObjectId>> {
    let commit = repo.find_object(id)?.peel_tags_to_end()?.try_into_commit()?;
    Ok(commit.parent_ids().map(|p| p.detach()).collect())
}

/// The name git records for one endpoint of a range argument under `--source`.
///
/// `handle_dotdot_1` passes the endpoint token — not the whole argument — to
/// `add_pending_object`, so `main~2..main` names its tip `main` and an omitted
/// endpoint (`..main`, `main..`) means `HEAD`.
fn range_endpoint(spec: &str, sep: &str, right: bool) -> String {
    let picked = match spec.split_once(sep) {
        Some((a, b)) => {
            if right {
                b
            } else {
                a
            }
        }
        None => spec,
    };
    if picked.is_empty() {
        "HEAD".to_string()
    } else {
        picked.to_string()
    }
}

/// Print `fatal: <msg>` on stderr and yield git's fatal exit code.
fn fatal(msg: &str) -> ExitCode {
    eprint!("fatal: {msg}");
    ExitCode::from(128)
}

/// The fatal `setup_revisions()` raises for an unresolvable revision argument,
/// shared with `git log` — `cmd_show` runs the same `cmd_log_init`. Already
/// carries its own `fatal: ` prefix, so it goes to stderr directly rather than
/// through [`fatal`].
fn bad_revision(repo: &gix::Repository, spec: &str) -> ExitCode {
    eprint!("{}", super::log::bad_revision_message_in(repo, spec));
    ExitCode::from(128)
}

// ---------------------------------------------------------------------------
// Pretty formats
// ---------------------------------------------------------------------------

enum Pretty {
    /// git's default `medium`: `commit`/`Merge`/`Author`/`Date` and an indented message.
    Medium,
    /// `<abbrev> <subject>` on one line.
    Oneline,
    /// A `--format=` string with `%` placeholders.
    User(String),
}

/// Parse a `--pretty`/`--format` value, or `None` when git would reject it with
/// `fatal: invalid --pretty format: <spec>`. git's rule: a `format:`/`tformat:`
/// prefix or any `%` placeholder is a user format; the empty string is an empty
/// user format (prints nothing); a known format name is that format; anything
/// else is invalid.
fn parse_pretty(spec: &str) -> Option<Pretty> {
    if let Some(fmt) = spec.strip_prefix("format:").or_else(|| spec.strip_prefix("tformat:")) {
        return Some(Pretty::User(fmt.to_string()));
    }
    match spec {
        "" => Some(Pretty::User(String::new())),
        "oneline" => Some(Pretty::Oneline),
        "medium" => Some(Pretty::Medium),
        _ if spec.contains('%') => Some(Pretty::User(spec.to_string())),
        _ => None,
    }
}

/// Reject any placeholder [`expand_format`] does not implement, so an unsupported
/// format fails loudly instead of expanding to something plausible but wrong.
fn check_format(fmt: &str) -> Result<()> {
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            continue;
        }
        match it.next() {
            Some('H' | 'h' | 'T' | 't' | 'P' | 'p' | 's' | 'n' | '%' | 'N') => {}
            Some('a') => match it.next() {
                Some('n' | 'e') => {}
                Some(x) => anyhow::bail!("unsupported format placeholder %a{x}"),
                None => anyhow::bail!("unsupported trailing % in format"),
            },
            // The column-control atoms — `%<(<N>)`, `%>(<N>)`, `%><(<N>)`, `%>>(<N>)`
            // and their `|`/`trunc`/`ltrunc`/`mtrunc` forms — and `%w(…)` are
            // validated where they are expanded: git prints a malformed one
            // literally rather than failing (see [`super::pretty_pad`]).
            Some('<' | '>' | 'w') => {}
            Some(x) => anyhow::bail!("unsupported format placeholder %{x}"),
            None => anyhow::bail!("unsupported trailing % in format"),
        }
    }
    Ok(())
}

/// Expand the placeholders accepted by [`check_format`] for `commit`.
///
/// The loop is git's `repo_format_commit_message()` driver: `%%` is expanded here
/// (so it never spends a pending padding field), every other placeholder goes
/// through [`expand_one`], and a `%<`/`%>` atom holds a column field open for
/// whichever placeholder comes next.
fn expand_format(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    fmt: &str,
    notes: &[super::notes::Tree],
) -> Result<()> {
    let chars: Vec<char> = fmt.chars().collect();
    let mut pad = PadState::default();
    let mut wrap = WrapState::default();
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
        if p == '%' {
            out.push(b'%');
            i += 1;
            continue;
        }
        i += 1;
        if pad.flush == FlushType::None {
            if !expand_one(out, commit, &chars, &mut i, p, notes, &mut pad, &mut wrap)? {
                out.push(b'%');
                i -= 1;
            }
            continue;
        }
        // `format_and_pad_commit()`: measure the placeholder's own output, in
        // display columns, and lay it out in the pending field.
        let padding = pad.padding;
        let mut local: Vec<u8> = Vec::new();
        let consumed = expand_one(&mut local, commit, &chars, &mut i, p, notes, &mut pad, &mut wrap)?;
        // `git show` has no `%C…`, so the colour-chaining half of
        // `format_and_pad_commit()` has nothing to chain here.
        pad.apply(out, local, padding, 0);
        if !consumed {
            out.push(b'%');
            i -= 1;
        }
    }
    wrap.rewrap_message_tail(out, 0, 0, 0);
    Ok(())
}

/// `format_commit_one()`: expand the single placeholder `p`, whose following
/// character is at `chars[*i]`. `false` is git's "consumed nothing", which makes
/// the caller print the `%` literally.
fn expand_one(
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    chars: &[char],
    i: &mut usize,
    p: char,
    notes: &[super::notes::Tree],
    pad: &mut PadState,
    wrap: &mut WrapState,
) -> Result<bool> {
    match p {
        // The column atoms expand to nothing and leave the field pending.
        '<' | '>' => {
            return Ok(match pad.parse(chars, *i - 1) {
                Some(consumed) => {
                    *i = *i - 1 + consumed;
                    true
                }
                None => false,
            })
        }
        // `%w(<width>,<indent1>,<indent2>)` re-wraps everything emitted after it.
        'w' => {
            return Ok(match wrap.parse_and_apply(out, chars, *i - 1) {
                Some(consumed) => {
                    *i = *i - 1 + consumed;
                    true
                }
                None => false,
            })
        }
        'H' => out.extend_from_slice(commit.id().to_string().as_bytes()),
        // `%N`: the raw note text, the only way a user format shows notes.
        'N' => out.extend_from_slice(&super::notes::format_display(
            commit.repo,
            notes,
            commit.id().detach(),
            true,
        )?),
        'h' => out.extend_from_slice(commit.id().shorten_or_id().to_string().as_bytes()),
        'T' => out.extend_from_slice(commit.tree_id()?.to_string().as_bytes()),
        't' => {
            out.extend_from_slice(commit.tree_id()?.shorten_or_id().to_string().as_bytes());
        }
        'P' => write_parents(out, commit, false),
        'p' => write_parents(out, commit, true),
        's' => out.extend_from_slice(&subject(commit.message_raw()?)),
        'n' => out.push(b'\n'),
        '%' => out.push(b'%'),
        'a' => {
            let author = commit.author()?;
            match chars.get(*i).copied() {
                Some('n') => out.extend_from_slice(author.name),
                Some('e') => out.extend_from_slice(author.email),
                _ => unreachable!("check_format rejected this already"),
            }
            *i += 1;
        }
        _ => unreachable!("check_format rejected this already"),
    }
    Ok(true)
}

/// Space-separated parent ids, abbreviated for `%p` and full for `%P`.
fn write_parents(out: &mut Vec<u8>, commit: &gix::Commit<'_>, abbrev: bool) {
    for (i, p) in commit.parent_ids().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        let text = if abbrev {
            p.shorten_or_id().to_string()
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
            break;
        }
        if !out.is_empty() {
            out.push(b' ');
        }
        out.extend_from_slice(line);
    }
    out
}

// ---------------------------------------------------------------------------
// Diff output format selection
// ---------------------------------------------------------------------------

/// The diff output formats requested on the command line, before git's
/// precedence rules are applied.
#[derive(Default, Clone, Copy)]
struct Formats {
    no_output: bool,
    name_only: bool,
    name_status: bool,
    raw: bool,
    stat: bool,
    numstat: bool,
    shortstat: bool,
    summary: bool,
    patch: bool,
}

/// `--name-only` together with `-s` and nothing else: git rejects this outright.
struct FormatConflict;

/// What actually gets rendered after git's precedence rules.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Selection {
    /// `-s` alone: no diff output and no separator after the message.
    Disabled,
    /// `--name-only`/`--name-status` win over every other diff format, whatever the
    /// order; `status` picks which of the two.
    Names { status: bool },
    /// Any combination of the block formats, rendered in git's fixed order.
    Blocks {
        raw: bool,
        numstat: bool,
        stat: bool,
        shortstat: bool,
        summary: bool,
        patch: bool,
    },
}

impl Formats {
    fn only_no_output() -> Self {
        Formats {
            no_output: true,
            ..Formats::default()
        }
    }

    fn any_set(self) -> bool {
        self.no_output
            || self.name_only
            || self.name_status
            || self.raw
            || self.stat
            || self.numstat
            || self.shortstat
            || self.summary
            || self.patch
    }

    /// The block formats, i.e. everything `--name-only`/`--name-status` overrides.
    fn any_block(self) -> bool {
        self.raw || self.stat || self.numstat || self.shortstat || self.summary || self.patch
    }

    /// Apply git's precedence: `-s` suppresses output only when it is the sole
    /// format; `--name-only` beats raw/stat/patch; naming both `-s` and
    /// `--name-only` with no third format is an error.
    fn resolve(mut self) -> Result<Selection, FormatConflict> {
        if !self.any_set() {
            self.patch = true;
        }
        let names = self.name_only || self.name_status;
        if self.no_output && names && !self.any_block() {
            return Err(FormatConflict);
        }
        if self.no_output && !names && !self.any_block() {
            return Ok(Selection::Disabled);
        }
        if names {
            return Ok(Selection::Names { status: self.name_status });
        }
        Ok(Selection::Blocks {
            raw: self.raw,
            numstat: self.numstat,
            stat: self.stat,
            shortstat: self.shortstat,
            summary: self.summary,
            patch: self.patch,
        })
    }
}

// ---------------------------------------------------------------------------
// Object rendering
// ---------------------------------------------------------------------------

/// The display knobs `git show` shares with `git log`, resolved once from config
/// and the command line.
struct DisplayOpts<'a> {
    /// `log.abbrevCommit` / `--abbrev-commit`: abbreviate the `commit <id>` line.
    abbrev_commit: bool,
    /// `log.date` / `--date=<mode>`: the format of the `Date:` line.
    date_mode: DateMode,
    /// `log.showRoot` / `--root`: whether a root commit's diff against the empty
    /// tree is shown (default true).
    show_root: bool,
    /// `--first-parent`: render a merge as a plain diff against its first parent
    /// rather than the dense combined (`--cc`) diff.
    first_parent: bool,
    /// The diff options the patch body is rendered with (`-U<n>`, `-w`, prefixes).
    patch: super::diff::PatchOpts,
    /// `--stat` width geometry (see [`StatWidths`]).
    stat: StatWidths,
    /// `--decorate` / `log.decorate`: the decoration style for the `commit <id>`
    /// and oneline headers. `Off` appends nothing.
    decorate: DecorateStyle,
    /// The commit→refs map behind `decorate`; `None` when decorations are off.
    decorations: Option<&'a Decorations>,
    /// `--use-mailmap` / `log.mailmap`: rewrites the `Author:` line through
    /// `.mailmap`. `None` shows the identity as the commit recorded it.
    mailmap: Option<&'a Mailmap>,
    /// The notes trees whose `Notes[ (<ref>)]:` block follows the message.
    notes: &'a [super::notes::Tree],
    /// `-z`: NUL instead of newline as the record terminator, and paths written
    /// raw rather than through `write_name_quoted()`. It reaches the header's own
    /// terminator too — `git show --name-status -z --format=%H` ends the id with a
    /// NUL — and, for a merge's combined record, the separator that follows it.
    z: bool,
    /// `revs->expand_tabs_in_log`, when `--expand-tabs[=<n>]`/`--no-expand-tabs`
    /// set one. `None` leaves the indented header formats on
    /// `expand_tabs_in_log_default`, which is 8.
    expand_tabs: Option<usize>,
}

/// A path as a record field: quoted the way `write_name_quoted()` does it, or raw
/// under `-z`, where the NUL delimiter makes quoting unnecessary.
fn name_bytes(path: &[u8], z: bool) -> Vec<u8> {
    if z {
        path.to_vec()
    } else {
        super::diff_files::quoted_name_bytes(path)
    }
}

/// The `Notes[ (<ref>)]:` blocks for `id`, or empty when notes are off.
///
/// git appends these to the message buffer, so the leading newline lands as the
/// blank line above the block after a `medium` message and as nothing extra
/// after a `oneline` subject — see [`super::notes::format_display`].
fn notes_block(
    repo: &gix::Repository,
    disp: &DisplayOpts<'_>,
    id: ObjectId,
) -> Result<Vec<u8>> {
    if disp.notes.is_empty() {
        return Ok(Vec::new());
    }
    super::notes::format_display(repo, disp.notes, id, false)
}

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

/// Pickaxe search (`-S`/`-G`): limits the shown diff to file pairs whose change
/// text matches, as git's `diffcore-pickaxe` does. Filtering is per file, so a
/// commit that touched several files shows only the ones that match.
struct Pickaxe {
    /// `-S<string>`: a filepair hits when the string's count differs between the
    /// two sides (a net add or remove).
    s: Option<String>,
    /// `-G<regex>`: a filepair hits when any added/removed line matches.
    g: Option<regex::bytes::Regex>,
}

impl Pickaxe {
    fn active(&self) -> bool {
        self.s.is_some() || self.g.is_some()
    }
}

/// Render the object `id` (named `spec` on the command line), peeling annotated
/// tags to their target after printing the tag header.
#[allow(clippy::too_many_arguments)]
fn show_one(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    spec: &str,
    id: ObjectId,
    pretty: &Pretty,
    selection: Selection,
    pathspecs: &[Vec<u8>],
    disp: &DisplayOpts<'_>,
    pickaxe: &Pickaxe,
    shown: &mut Vec<ObjectId>,
    source: Option<&str>,
    shown_one: &mut bool,
    // `line_log_queue_pairs()`' output under `-L`: `Some` replaces the whole diff
    // section with the tracked file pairs, clipped to their ranges.
    line_log_pairs: Option<&[(line_log::Pair, Vec<line_log::Range>)]>,
) -> Result<()> {
    let mut obj = repo.find_object(id)?;
    loop {
        match obj.kind {
            Kind::Blob => {
                out.extend_from_slice(&obj.data);
                break;
            }
            Kind::Tree => {
                // `case OBJ_TREE:` in `cmd_show` — `if (rev.shown_one) putchar('\n')`
                // before the header, `rev.shown_one = 1` after the listing. Both
                // halves matter: without the first a tree run into a preceding
                // record, without the second whatever followed a tree ran into it.
                // `OBJ_BLOB` does neither, which is why a blob has no separator.
                if *shown_one {
                    out.push(b'\n');
                }
                show_tree(out, &obj, spec)?;
                *shown_one = true;
                break;
            }
            Kind::Commit => {
                // git prints a given commit at most once (the SHOWN flag).
                if shown.contains(&obj.id) {
                    break;
                }
                shown.push(obj.id);
                let commit = obj.try_into_commit()?;
                show_commit(repo, out, &commit, pretty, selection, pathspecs, disp, pickaxe, source, shown_one, line_log_pairs)?;
                break;
            }
            Kind::Tag => {
                let target = show_tag(out, &obj, disp.date_mode)?;
                obj = repo.find_object(target)?;
            }
        }
    }
    Ok(())
}

/// `--source`: git's `show_log` prints `\t<source>` right after the commit hash
/// on the built-in header formats. A no-op when `--source` is off, and never
/// called for user formats, which git leaves bare.
fn write_source(out: &mut Vec<u8>, source: Option<&str>) {
    if let Some(src) = source {
        out.push(b'\t');
        out.extend_from_slice(src.as_bytes());
    }
}

/// `--decorate`: append ` (HEAD -> main, tag: v1)` after the commit hash, in the
/// selected style. Rendered by `git log`'s decoration formatter so the two
/// commands emit the same bytes. Never colorized: `git show` here is always the
/// `--no-color` case.
fn write_decorations(out: &mut Vec<u8>, id: &gix::hash::oid, disp: &DisplayOpts<'_>) {
    let (Some(decos), true) = (disp.decorations, disp.decorate != DecorateStyle::Off) else {
        return;
    };
    super::log::format_decorations(
        out,
        decos,
        &id.to_owned(),
        disp.decorate == DecorateStyle::Full,
        &super::color::DecorateColors::disabled(),
        true,
    );
}

/// `tree <name>` header followed by the top-level entry names. git echoes the name
/// as it was written on the command line, not the resolved object id.
fn show_tree(out: &mut Vec<u8>, obj: &gix::Object<'_>, name: &str) -> Result<()> {
    out.extend_from_slice(b"tree ");
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b"\n\n");
    for entry in TreeRefIter::from_bytes(&obj.data, obj.id.kind()) {
        let entry = entry?;
        out.extend_from_slice(entry.filename);
        if entry.mode.is_tree() {
            out.push(b'/');
        }
        out.push(b'\n');
    }
    Ok(())
}

/// Annotated-tag header. Returns the id of the object the tag points to so the
/// caller can continue rendering the target.
fn show_tag(out: &mut Vec<u8>, obj: &gix::Object<'_>, date_mode: DateMode) -> Result<ObjectId> {
    let tag = obj.try_to_tag_ref()?;

    out.extend_from_slice(b"tag ");
    out.extend_from_slice(tag.name);
    out.push(b'\n');

    if let Some(tagger) = tag.tagger()? {
        out.extend_from_slice(b"Tagger: ");
        out.extend_from_slice(tagger.name);
        out.extend_from_slice(b" <");
        out.extend_from_slice(tagger.email);
        out.extend_from_slice(b">\n");
        let t = tagger.time()?;
        let date = format_date(t.seconds, t.offset, date_mode);
        writeln!(out, "Date:   {date}")?;
    }

    out.push(b'\n');
    // The tag message is printed verbatim (not indented), followed by a blank line.
    out.extend_from_slice(tag.message);
    if !tag.message.ends_with(b"\n") {
        out.push(b'\n');
    }
    out.push(b'\n');

    Ok(tag.target())
}

/// The commit header in the selected pretty format, then the separator, then the
/// selected diff output against the first parent.
#[allow(clippy::too_many_arguments)]
fn show_commit(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    pretty: &Pretty,
    selection: Selection,
    pathspecs: &[Vec<u8>],
    disp: &DisplayOpts<'_>,
    pickaxe: &Pickaxe,
    source: Option<&str>,
    shown_one: &mut bool,
    line_log_pairs: Option<&[(line_log::Pair, Vec<line_log::Range>)]>,
) -> Result<()> {
    let parents: Vec<_> = commit.parent_ids().collect();
    let is_merge = parents.len() > 1;
    // An empty user format (`--format=`) prints no header at all, and git then
    // omits the blank line that would separate the header from the diff.
    let header_empty = matches!(pretty, Pretty::User(f) if f.is_empty());

    // Resolve the file-level changes up front — before the header — so `-S`/`-G`
    // can suppress the ENTIRE commit (header included) when the pickaxe matches no
    // file, exactly as git does. `files` is computed only when a diff would be
    // shown (a real diff selection, and either a non-root commit or `--root`).
    let diff_shown =
        selection != Selection::Disabled && !(parents.is_empty() && !disp.show_root);
    // The pickaxe applies to the first-parent / non-merge path; a merge's combined
    // `--cc` diff is never paired with pickaxe by git-fuzzy and is left unfiltered.
    let pickaxe_path = pickaxe.active() && !(is_merge && !disp.first_parent);
    let mut queue_nonempty = false;
    let files: Vec<FileChange> = if line_log_pairs.is_some() {
        // `-L` renders `line_log_queue_pairs()`' pairs, not the commit's own change
        // set, so none of the collection below runs.
        Vec::new()
    } else if diff_shown {
        let mut f = collect_changes(
            repo,
            commit,
            parents.first().map(|p| p.detach()),
            &disp.patch,
        )?;
        if !pathspecs.is_empty() {
            let specs = super::log::PathspecMatcher::new(repo, pathspecs)?;
            f.retain(|c| specs.matches(&c.path));
        }
        if pickaxe_path {
            // Keep only files whose own change text matches, testing each file's
            // patch exactly as `git log` tests a commit's patch.
            f.retain(|c| {
                let mut buf = Vec::new();
                emit_patch(repo, &mut buf, c).is_ok()
                    && super::log::pickaxe_hit(&buf, pickaxe.s.as_deref(), pickaxe.g.as_ref())
            });
        }
        // `log_tree_diff_flush()` tests the queue for emptiness here, before
        // `diff_flush()` re-renders it quietly under a whitespace rule and drops the
        // pairs whose patch came out empty. A whitespace-only commit therefore still
        // separates its message from the diff it no longer prints.
        queue_nonempty = !f.is_empty();
        if disp.patch.ws != super::diff::Whitespace::Keep {
            f.retain(reports_change);
        }
        f
    } else {
        Vec::new()
    };
    if pickaxe_path && diff_shown && files.is_empty() && line_log_pairs.is_none() {
        return Ok(());
    }

    // git's `show_log`: a separator format (everything but the terminator formats
    // `oneline` and `tformat:`) puts a blank line before every record but the
    // first, which is what separates two commits of `git show A..B`. The
    // terminator formats already ended the previous record with a newline.
    if *shown_one && matches!(pretty, Pretty::Medium) {
        out.push(b'\n');
    }
    *shown_one = true;

    match pretty {
        Pretty::Oneline => {
            // `--pretty=oneline` prints the full object name; only `--oneline`,
            // which is `--pretty=oneline --abbrev-commit`, shortens it.
            if disp.abbrev_commit {
                out.extend_from_slice(commit.id().shorten_or_id().to_string().as_bytes());
            } else {
                out.extend_from_slice(commit.id().to_string().as_bytes());
            }
            write_source(out, source);
            write_decorations(out, commit.id().as_ref(), disp);
            out.push(b' ');
            out.extend_from_slice(&subject(commit.message_raw()?));
            out.extend_from_slice(&notes_block(repo, disp, commit.id().detach())?);
            out.push(b'\n');
        }
        Pretty::User(fmt) => {
            expand_format(out, commit, fmt, disp.notes)?;
            // A `tformat` (the default for `--format=`) terminates each non-empty
            // entry with the record terminator — a newline, or NUL under `-z`, which
            // is `show_log()` writing `opt->diffopt.line_termination`.
            if !fmt.is_empty() {
                out.push(if disp.z { b'\0' } else { b'\n' });
            }
        }
        Pretty::Medium => {
            // `log.abbrevCommit`/`--abbrev-commit` shortens the `commit` line; the
            // `Merge:` parents are always abbreviated, as in git.
            if disp.abbrev_commit {
                write!(out, "commit {}", commit.id().shorten_or_id())?;
            } else {
                write!(out, "commit {}", commit.id())?;
            }
            write_source(out, source);
            write_decorations(out, commit.id().as_ref(), disp);
            out.push(b'\n');
            if is_merge {
                out.extend_from_slice(b"Merge:");
                for p in &parents {
                    out.push(b' ');
                    out.extend_from_slice(p.shorten_or_id().to_string().as_bytes());
                }
                out.push(b'\n');
            }

            let author = commit.author()?;
            // `--use-mailmap`/`log.mailmap` resolves the identity here, which is
            // the one place git's built-in formats print it (`pp_user_info`).
            super::log::write_person(out, b"Author: ", &author, disp.mailmap);
            let t = author.time()?;
            let date = format_date(t.seconds, t.offset, disp.date_mode);
            writeln!(out, "Date:   {date}")?;
            out.push(b'\n');

            // Message, each line indented four spaces (blank lines become four
            // spaces), with trailing blank lines stripped — and its tabs expanded
            // against the message's own left edge, which is what keeps whatever
            // the author lined up in columns from shifting under the indent.
            // Shared with `git log`, whose `medium`/`full`/`fuller` do the same.
            super::log::indent_message(out, commit.message_raw()?, disp.expand_tabs.unwrap_or(8));
            out.extend_from_slice(&notes_block(repo, disp, commit.id().detach())?);
        }
    }

    if selection == Selection::Disabled {
        return Ok(());
    }

    // `log_tree_diff` short-circuits under `-L`: it flushes the queued pairs and
    // returns, so the merge and root-commit rules below never apply.
    if let Some(pairs) = line_log_pairs {
        if pairs.is_empty() {
            return Ok(());
        }
        if !header_empty && !matches!(pretty, Pretty::Oneline) {
            out.push(b'\n');
        }
        match selection {
            Selection::Disabled => {}
            Selection::Names { status } => {
                for (pair, _) in pairs {
                    if status {
                        out.push(line_log_change(repo, pair).status);
                        out.push(b'\t');
                    }
                    out.extend_from_slice(&pair.path);
                    out.push(b'\n');
                }
            }
            Selection::Blocks { raw, patch, .. } => {
                let mut wrote_block = false;
                if raw {
                    let files: Vec<FileChange> =
                        pairs.iter().map(|(p, _)| line_log_change(repo, p)).collect();
                    emit_raw(repo, out, &files, disp.z)?;
                    wrote_block = true;
                }
                if patch {
                    if wrote_block {
                        out.push(b'\n');
                    }
                    out.extend_from_slice(&super::diff::line_range_patch(repo, pairs, 3)?);
                }
            }
        }
        return Ok(());
    }

    // `log.showRoot=false` (with no `--root`) suppresses a root commit's diff
    // against the empty tree: the header prints, but no separator and no diff.
    if parents.is_empty() && !disp.show_root {
        return Ok(());
    }

    if is_merge && !disp.first_parent {
        // git shows a blank line after a merge's message regardless of format,
        // then — by default — the dense combined diff (`--cc`) against all
        // parents, plus `--stat` (against the first parent) when requested. The
        // empty user format prints neither the blank line nor a header.
        // `--first-parent` opts out: the merge falls through to the plain
        // single-parent path below, diffing against `parents[0]` like any commit.
        //
        // A combined record's separator is the record terminator, so `-z` makes it a
        // NUL — unlike the single-parent path below, where the blank line stays a
        // newline even under `-z`.
        if !header_empty {
            out.push(if disp.z { b'\0' } else { b'\n' });
        }
        // A merge's name list is the *combined* one: one status letter per parent,
        // and only the paths that differ from every parent (`diff_tree_combined()`'s
        // dense filter). `--name-only` prints the paths alone.
        if let Selection::Names { status } = selection {
            let ps: Vec<String> = pathspecs
                .iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
            let parent_ids: Vec<ObjectId> = parents.iter().map(|p| p.detach()).collect();
            let sep = if disp.z { b'\0' } else { b'\t' };
            let end = if disp.z { b'\0' } else { b'\n' };
            for (path, letters) in
                super::diff::merge_combined_names(repo, commit.id().detach(), &parent_ids, &ps)?
            {
                if status {
                    out.extend_from_slice(letters.as_bytes());
                    out.push(sep);
                }
                out.extend_from_slice(&name_bytes(&path, disp.z));
                out.push(end);
            }
            return Ok(());
        }
        if let Selection::Blocks { stat, patch, .. } = selection {
            let mut wrote = false;
            if stat {
                emit_stat(out, &files, &disp.stat)?;
                wrote = true;
            }
            if patch {
                // Dense combined diff of the merge's tree against every
                // parent tree, rendered by the shared `diff --cc` engine.
                let result_tree = commit.tree()?;
                let mut parent_trees = Vec::with_capacity(parents.len());
                for p in &parents {
                    parent_trees.push(repo.find_commit(p.detach())?.tree()?);
                }
                let ps: Vec<String> = pathspecs
                    .iter()
                    .map(|p| String::from_utf8_lossy(p).into_owned())
                    .collect();
                let cc = super::diff::combined_trees_patch(
                    repo,
                    &result_tree,
                    &parent_trees,
                    &ps,
                    3,
                )?;
                if wrote && !cc.is_empty() {
                    out.push(b'\n');
                }
                out.extend_from_slice(&cc);
            }
        }
        return Ok(());
    }

    // A pathspec that matched nothing leaves the message with no diff and, like git,
    // no trailing separator.
    if !queue_nonempty {
        return Ok(());
    }

    // Separator between the message and the diff output. `--oneline` and the empty
    // user format get none; a combined stat-plus-patch gets `---`; otherwise a
    // blank line.
    if !header_empty {
        match (pretty, selection) {
            (Pretty::Oneline, _) => {}
            (
                _,
                Selection::Blocks {
                    stat: true,
                    patch: true,
                    ..
                },
            ) => out.extend_from_slice(b"---\n"),
            _ => out.push(b'\n'),
        }
    }

    match selection {
        Selection::Disabled => {}
        Selection::Names { status } => {
            // Under `-z` every field ends in a NUL and the paths are written raw:
            // `diff_flush_name()` skips `write_name_quoted()` when there is nothing a
            // newline-delimited reader could confuse.
            let sep = if disp.z { b'\0' } else { b'\t' };
            let end = if disp.z { b'\0' } else { b'\n' };
            for f in &files {
                if status {
                    out.push(f.status);
                    // A rename names its similarity index and both paths.
                    if let Some(source) = &f.source {
                        out.extend_from_slice(format!("{:03}", f.score).as_bytes());
                        out.push(sep);
                        out.extend_from_slice(&name_bytes(source, disp.z));
                    }
                    out.push(sep);
                }
                out.extend_from_slice(&name_bytes(&f.path, disp.z));
                out.push(end);
            }
        }
        Selection::Blocks {
            raw,
            numstat,
            stat,
            shortstat,
            summary,
            patch,
        } => {
            let mut wrote_block = false;
            // `diff_flush()`'s fixed order: raw, numstat, stat, shortstat, summary,
            // then the patch.
            if raw {
                emit_raw(repo, out, &files, disp.z)?;
                wrote_block = true;
            }
            if numstat {
                emit_numstat(out, &files);
                wrote_block = true;
            }
            // `diff_flush()` tests the two bits separately, so `--stat --shortstat`
            // prints the stat block and then a second summary line.
            if stat {
                emit_stat(out, &files, &disp.stat)?;
                wrote_block = true;
            }
            if shortstat {
                emit_shortstat(out, &files)?;
                wrote_block = true;
            }
            if summary {
                emit_summary(out, &files)?;
                wrote_block = true;
            }
            if patch {
                if wrote_block {
                    out.push(b'\n');
                }
                // The patch body comes from the shared `git diff` pipeline (the same
                // one `git log -p` renders through), so every diff option it takes —
                // `-w`, `-U<n>`, `--full-index`, the prefixes — applies here too, and
                // the two commands stay byte-identical. The pickaxe path above still
                // renders per file, since it filters on each file's own patch.
                let specs: Vec<String> = pathspecs
                    .iter()
                    .map(|p| String::from_utf8_lossy(p).into_owned())
                    .collect();
                out.extend_from_slice(
                    &super::diff::commit_patches(
                        repo,
                        &[(commit.id, parents.first().map(|p| p.detach()))],
                        &disp.patch,
                        &specs,
                        false,
                    )?
                    .pop()
                    .unwrap_or_default(),
                );
            }
        }
    }

    Ok(())
}

/// git limits a commit's diff to paths matching the pathspecs after `--`. Without
/// pathspec magic (`:(glob)`, `:!`, …), a pathspec matches a path when they are
/// equal or the path lies under the pathspec directory. `.` matches everything.
// ---------------------------------------------------------------------------
// Change collection
// ---------------------------------------------------------------------------

/// One file-level change, with everything the four output formats need resolved
/// once so the blob contents are read at most a single time.
/// A `-L` file pair as the `--raw` renderer wants it. Only the identity fields are
/// filled: the content-bearing ones are unused by `emit_raw`, and `-L` never routes
/// its patch through this record.
fn line_log_change(repo: &gix::Repository, pair: &line_log::Pair) -> FileChange {
    let null = repo.object_hash().null();
    let mode = |s: line_log::Side| {
        s.map(|(_, k)| u32::from(gix::objs::tree::EntryMode::from(k).value()))
    };
    // `diff_resolve_rename_copy()` re-derives the status from the two filespecs of
    // the `diff_filepair_dup()` the `-L` queue holds, which carries no rename flag —
    // so a pair whose sides name different files is still a plain `M`, printed under
    // its pre-image path.
    let (status, path) = match (pair.old, pair.new) {
        (None, _) => (b'A', &pair.path),
        (_, None) => (b'D', &pair.old_path),
        _ => (b'M', &pair.old_path),
    };
    FileChange {
        path: path.to_vec(),
        status,
        source: None,
        score: 0,
        old_mode: mode(pair.old),
        new_mode: mode(pair.new),
        old_id: pair.old.map_or(null, |(id, _)| id),
        new_id: pair.new.map_or(null, |(id, _)| id),
        old_content: Vec::new(),
        new_content: Vec::new(),
        old_is_sub: matches!(pair.old, Some((_, EntryKind::Commit))),
        new_is_sub: matches!(pair.new, Some((_, EntryKind::Commit))),
        is_binary: false,
        mode_only: false,
        added: 0,
        deleted: 0,
    }
}

struct FileChange {
    path: Vec<u8>,
    /// `A`, `D`, `M`, `T` or `R`, as used by `--raw`.
    status: u8,
    /// The path the content came from, for a rename. `None` for everything else.
    source: Option<Vec<u8>>,
    /// `similarity_index()` of the rename, in percent. Meaningless without `source`.
    score: u32,
    /// Octal entry modes; `None` on the side where the path does not exist.
    old_mode: Option<u32>,
    new_mode: Option<u32>,
    old_id: ObjectId,
    new_id: ObjectId,
    old_content: Vec<u8>,
    new_content: Vec<u8>,
    old_is_sub: bool,
    new_is_sub: bool,
    is_binary: bool,
    /// Set when only the mode changed, which suppresses the `index` line and hunks.
    mode_only: bool,
    added: usize,
    deleted: usize,
}

/// Diff `commit`'s tree against `parent`'s (or the empty tree for a root commit),
/// dropping the directory entries gix reports alongside the files it recurses into.
fn collect_changes(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    parent: Option<ObjectId>,
    // The diffcore knobs (`-M`/`-C`/`-B`) and the whitespace rule the tallies are
    // computed under, so a whitespace-only change reports nothing.
    opts: &super::diff::PatchOpts,
) -> Result<Vec<FileChange>> {
    let ws = opts.ws;
    let new_tree = commit.tree()?;
    let old_tree = match parent {
        Some(pid) => Some(repo.find_object(pid)?.try_into_commit()?.tree()?),
        None => None,
    };

    let mut changes = repo.diff_tree_to_tree(
        old_tree.as_ref(),
        Some(&new_tree),
        gix::diff::Options::default(),
    )?;
    changes.sort_by(|a, b| change_path(a).cmp(change_path(b)));

    let mut out = Vec::with_capacity(changes.len());
    for change in &changes {
        if let Some(f) = prepare_change(repo, change, ws)? {
            out.push(f);
        }
    }
    detect_renames(repo, &mut out, opts)?;
    Ok(out)
}

/// `diffcore_rename()`: pair each deletion with an addition that carries the same (or
/// similar enough) content, so a moved file is one `R` entry rather than a `D` plus an
/// `A`.
///
/// `git show` is a porcelain, so `init_diff_ui_defaults()` has rename detection on
/// with git's default 50% similarity; `diff.renames` and `diff.renameLimit` move it.
/// The pass is the same port `git diff` uses, so the pairing and the similarity
/// indices agree between the two commands.
fn detect_renames(
    repo: &gix::Repository,
    files: &mut Vec<FileChange>,
    popts: &super::diff::PatchOpts,
) -> Result<()> {
    let cfg = repo.config_snapshot();
    let detect = popts.renames.unwrap_or_else(|| {
        super::diffcore_rename::config_rename(
            cfg.string("diff.renames").as_deref().map(|v| v.as_bstr()),
        )
    });
    // `-B` runs on its own: `diffcore_std()` breaks rewrites whether or not rename
    // detection follows, and a break with no rename pass still reports the split.
    let wants_break = popts.break_opt != -1;
    if (detect == 0 && !wants_break)
        || (!wants_break && !files.iter().any(|f| f.status == b'A' || f.status == b'D'))
    {
        return Ok(());
    }
    let opts = super::diffcore_rename::Options {
        detect_rename: detect,
        rename_score: popts.rename_score,
        find_copies_harder: popts.find_copies_harder,
        break_opt: popts.break_opt,
        rename_limit: cfg
            .integer("diff.renameLimit")
            .unwrap_or(super::diffcore_rename::DEFAULT_RENAME_LIMIT),
        hash_kind: repo.object_hash(),
        ..Default::default()
    };
    let ws = popts.ws;

    let mut q = super::diffcore_rename::Queue::default();
    for f in files.iter() {
        let one = q.add_spec(super::diffcore_rename::FileSpec::new(
            f.path.clone().into(),
            f.old_mode.unwrap_or(0),
            f.old_id,
            f.old_mode.is_some(),
        ));
        let two = q.add_spec(super::diffcore_rename::FileSpec::new(
            f.path.clone().into(),
            f.new_mode.unwrap_or(0),
            f.new_id,
            f.new_mode.is_some(),
        ));
        let idx = q.add_pair(one, two);
        q.pairs[idx].status = f.status;
    }

    let mut content = ShowContent { repo };
    super::diffcore_rename::run(&mut q, &opts, &mut content);
    super::diffcore_rename::resolve_rename_copy(&mut q);

    // Rebuild the list from the resolved queue: a rename replaces the deletion and the
    // addition it was made of, and everything else comes back unchanged.
    let mut rebuilt = Vec::with_capacity(q.pairs.len());
    for pair in &q.pairs {
        let source = &q.specs[pair.one];
        let dest = &q.specs[pair.two];
        let status = if pair.status == 0 { b'M' } else { pair.status };
        if !matches!(status, b'R' | b'C') {
            // Not a rename: the entry this pair was built from already has its
            // contents and counts, and both of its sides carry that one path. A `-B`
            // rewrite that stayed a modification carries a score, which `--summary`
            // prints as its ` rewrite ... (n%)` line.
            if let Some(at) = files.iter().position(|f| f.path == dest.path.as_slice()) {
                let mut kept = files.swap_remove(at);
                kept.score = super::diffcore_rename::similarity_index(pair.score);
                rebuilt.push(kept);
            }
            continue;
        }
        let old_is_sub = source.mode & 0o170000 == 0o160000;
        let new_is_sub = dest.mode & 0o170000 == 0o160000;
        let mut f = FileChange {
            path: dest.path.to_vec(),
            status,
            source: Some(source.path.to_vec()),
            score: super::diffcore_rename::similarity_index(pair.score),
            old_mode: Some(source.mode),
            new_mode: Some(dest.mode),
            old_id: source.oid,
            new_id: dest.oid,
            old_content: content_of(repo, source.oid, old_is_sub)?,
            new_content: content_of(repo, dest.oid, new_is_sub)?,
            old_is_sub,
            new_is_sub,
            is_binary: false,
            // A pure rename has nothing to show below the header, exactly as a
            // mode-only change has not.
            mode_only: source.oid == dest.oid,
            added: 0,
            deleted: 0,
        };
        fill_counts(&mut f, ws)?;
        rebuilt.push(f);
    }
    rebuilt.sort_by(|a, b| a.path.cmp(&b.path));
    *files = rebuilt;
    Ok(())
}

/// Reads a filespec's blob for [`diffcore_rename`]; every side names an object in the
/// database here, so this is `diff_populate_filespec()` reduced to an odb lookup.
struct ShowContent<'a> {
    repo: &'a gix::Repository,
}

impl super::diffcore_rename::Content for ShowContent<'_> {
    fn size(&mut self, spec: &super::diffcore_rename::FileSpec) -> Option<u64> {
        let header = self.repo.find_header(spec.oid).ok()?;
        (header.kind() == gix::object::Kind::Blob).then(|| header.size())
    }

    fn data(&mut self, spec: &super::diffcore_rename::FileSpec) -> Option<Vec<u8>> {
        self.repo.find_object(spec.oid).ok().map(|o| o.detach().data)
    }
}

/// Turn one gix change into a [`FileChange`], or `None` for the directory entries
/// git does not report (gix emits those *and* recurses into them).
fn prepare_change(
    repo: &gix::Repository,
    change: &ChangeDetached,
    ws: super::diff::Whitespace,
) -> Result<Option<FileChange>> {
    let null = ObjectId::null(repo.object_hash());
    let mut f = match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            if entry_mode.is_tree() {
                return Ok(None);
            }
            let is_sub = entry_mode.is_commit();
            FileChange {
                path: location.to_vec(),
                status: b'A',
                source: None,
                score: 0,
                old_mode: None,
                new_mode: Some(entry_mode.value().into()),
                old_id: null,
                new_id: *id,
                old_content: Vec::new(),
                new_content: content_of(repo, *id, is_sub)?,
                old_is_sub: false,
                new_is_sub: is_sub,
                is_binary: false,
                mode_only: false,
                added: 0,
                deleted: 0,
            }
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
            let is_sub = entry_mode.is_commit();
            FileChange {
                path: location.to_vec(),
                status: b'D',
                source: None,
                score: 0,
                old_mode: Some(entry_mode.value().into()),
                new_mode: None,
                old_id: *id,
                new_id: null,
                old_content: content_of(repo, *id, is_sub)?,
                new_content: Vec::new(),
                old_is_sub: is_sub,
                new_is_sub: false,
                is_binary: false,
                mode_only: false,
                added: 0,
                deleted: 0,
            }
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
            let old_is_sub = previous_entry_mode.is_commit();
            let new_is_sub = entry_mode.is_commit();
            let status = if type_class(previous_entry_mode.kind()) == type_class(entry_mode.kind()) {
                b'M'
            } else {
                b'T'
            };
            FileChange {
                path: location.to_vec(),
                status,
                source: None,
                score: 0,
                old_mode: Some(previous_entry_mode.value().into()),
                new_mode: Some(entry_mode.value().into()),
                old_id: *previous_id,
                new_id: *id,
                old_content: content_of(repo, *previous_id, old_is_sub)?,
                new_content: content_of(repo, *id, new_is_sub)?,
                old_is_sub,
                new_is_sub,
                is_binary: false,
                mode_only: previous_id == id,
                added: 0,
                deleted: 0,
            }
        }
        // Never produced: rewrite tracking is disabled via Options::default().
        ChangeDetached::Rewrite { .. } => bail!("rename/copy detection is not supported"),
    };

    fill_counts(&mut f, ws)?;
    Ok(Some(f))
}

/// Whether a pair still has something to report once a whitespace rule has been
/// applied — `diff_flush_patch_quietly()`'s test, stated over the change list: a
/// creation, deletion, mode change, rename or binary difference always prints a
/// header, and everything else survives only if lines actually differ.
fn reports_change(f: &FileChange) -> bool {
    f.old_mode.is_none()
        || f.new_mode.is_none()
        || f.old_mode != f.new_mode
        || f.source.is_some()
        || (f.is_binary && f.old_id != f.new_id)
        || f.added != 0
        || f.deleted != 0
}

/// The per-file tallies every format needs: whether the pair is binary, and the
/// insert/delete counts when it is not.
fn fill_counts(f: &mut FileChange, ws: super::diff::Whitespace) -> Result<()> {
    f.is_binary = (!f.old_is_sub && is_binary(&f.old_content))
        || (!f.new_is_sub && is_binary(&f.new_content));
    if !f.is_binary && !f.mode_only {
        let (added, deleted) = count_changed_lines_ws(&f.old_content, &f.new_content, ws)?;
        f.added = added;
        f.deleted = deleted;
    }
    Ok(())
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

// ---------------------------------------------------------------------------
// --raw
// ---------------------------------------------------------------------------

/// `:<old mode> <new mode> <old sha> <new sha> <status>\t<path>`.
fn emit_raw(repo: &gix::Repository, out: &mut Vec<u8>, files: &[FileChange], z: bool) -> Result<()> {
    // `-z` swaps the field separator and the record terminator for NULs and drops
    // the path quoting, exactly as in the name-status form.
    let sep = if z { b'\0' } else { b'\t' };
    let end = if z { b'\0' } else { b'\n' };
    for f in files {
        write!(out, ":{:06o} {:06o} ", f.old_mode.unwrap_or(0), f.new_mode.unwrap_or(0))?;
        let old = short_oid(repo, &f.old_id, f.old_mode.is_none() || f.old_is_sub)?;
        let new = short_oid(repo, &f.new_id, f.new_mode.is_none() || f.new_is_sub)?;
        out.extend_from_slice(old.as_bytes());
        out.push(b' ');
        out.extend_from_slice(new.as_bytes());
        out.push(b' ');
        out.push(f.status);
        // `diff_flush_raw()`: a rename carries its similarity index and both paths.
        if let Some(source) = &f.source {
            write!(out, "{:03}", f.score)?;
            out.push(sep);
            out.extend_from_slice(&name_bytes(source, z));
        }
        out.push(sep);
        out.extend_from_slice(&name_bytes(&f.path, z));
        out.push(end);
    }
    Ok(())
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
    // `count` files get a bar line, but the scan for the geometry also stops there.
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
        max_len = max_len.max(display_width(&stat_name(f)) as i64);
        if f.is_binary {
            // `"Bin XXX -> YYY bytes"`: 14 fixed chars plus each size's decimal width.
            let w = 14 + decimal_width(f.new_content.len()) as i64
                + decimal_width(f.old_content.len()) as i64;
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
        let display = stat_name(f);
        let (prefix, name) = elide_name(&display, name_width);
        let padding = name_width.saturating_sub(prefix.len() + display_width(name));
        out.push(b' ');
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&b" ".repeat(padding));
        out.extend_from_slice(b" | ");

        if f.is_binary {
            // For binaries the counts are byte sizes, not lines.
            let old_size = f.old_content.len();
            let new_size = f.new_content.len();
            write!(out, "{:>width$}", "Bin", width = number_width)?;
            if old_size == 0 && new_size == 0 {
                out.push(b'\n');
            } else {
                writeln!(out, " {old_size} -> {new_size} bytes")?;
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

    let n = files.len();
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

/// `--numstat` (`show_numstat()`): added, deleted, name — with `-` counts for a
/// binary pair, and the rename form for a moved file.
fn emit_numstat(out: &mut Vec<u8>, files: &[FileChange]) {
    for f in files {
        if f.is_binary {
            out.extend_from_slice(b"-\t-\t");
        } else {
            out.extend_from_slice(format!("{}\t{}\t", f.added, f.deleted).as_bytes());
        }
        out.extend_from_slice(&stat_name(f));
        out.push(b'\n');
    }
}

/// `--shortstat` (`show_shortstats()`): the `--stat` summary line on its own.
/// Binary pairs contribute no insertions or deletions, exactly as in the full block.
fn emit_shortstat(out: &mut Vec<u8>, files: &[FileChange]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let (added, deleted) = files
        .iter()
        .filter(|f| !f.is_binary)
        .fold((0usize, 0usize), |(a, d), f| (a + f.added, d + f.deleted));
    write!(out, " {} file{} changed", files.len(), plural(files.len()))?;
    if added != 0 || deleted == 0 {
        write!(out, ", {added} insertion{}(+)", plural(added))?;
    }
    if deleted != 0 || added == 0 {
        write!(out, ", {deleted} deletion{}(-)", plural(deleted))?;
    }
    out.push(b'\n');
    Ok(())
}

/// `diff_summary()` (diff.c): the `create`/`delete`/`mode change`/`rename` lines that
/// follow the diffstat.
fn emit_summary(out: &mut Vec<u8>, files: &[FileChange]) -> Result<()> {
    for f in files {
        match (f.old_mode, f.new_mode, &f.source) {
            // `show_rename_copy()`: the paired name, then the mode-change line with
            // its name suppressed — the rename line above already carried one.
            (_, _, Some(source)) => {
                writeln!(
                    out,
                    " {} {} ({}%)",
                    if f.status == b'C' { "copy" } else { "rename" },
                    String::from_utf8_lossy(&super::diff_pairs::pprint_rename(source, &f.path)),
                    f.score
                )?;
                summary_mode_change(out, f, false)?;
            }
            (None, Some(new), None) => summary_mode_name(out, "create", new, &f.path)?,
            (Some(old), None, None) => summary_mode_name(out, "delete", old, &f.path)?,
            // `diff_summary()`'s default arm: a `-B` rewrite that stayed a
            // modification announces itself and suppresses the mode-change name.
            _ if f.score != 0 => {
                out.extend_from_slice(b" rewrite ");
                out.extend_from_slice(&super::diff_files::quoted_name_bytes(&f.path));
                write!(out, " ({}%)\n", f.score)?;
                summary_mode_change(out, f, false)?;
            }
            _ => summary_mode_change(out, f, true)?,
        }
    }
    Ok(())
}

/// `show_file_mode_name()`: ` create mode <mode> <path>` / ` delete mode …`.
fn summary_mode_name(out: &mut Vec<u8>, verb: &str, mode: u32, path: &[u8]) -> Result<()> {
    write!(out, " {verb} mode {mode:06o} ")?;
    out.extend_from_slice(&super::diff_files::quoted_name_bytes(path));
    out.push(b'\n');
    Ok(())
}

/// `show_mode_change()`: the ` mode change <old> => <new>` line, named only when no
/// other summary line for this pair printed the path.
fn summary_mode_change(out: &mut Vec<u8>, f: &FileChange, show_name: bool) -> Result<()> {
    let (Some(old), Some(new)) = (f.old_mode, f.new_mode) else {
        return Ok(());
    };
    if old == new {
        return Ok(());
    }
    write!(out, " mode change {old:06o} => {new:06o}")?;
    if show_name {
        out.push(b' ');
        out.extend_from_slice(&super::diff_files::quoted_name_bytes(&f.path));
    }
    out.push(b'\n');
    Ok(())
}

/// `"s"` unless the count is one, for the summary lines' plurals.
fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The name the stat formats print for a file: a rename shows both sides through
/// `pprint_rename()`, which factors out a shared prefix and suffix
/// (`dir/{old => new}.txt`) and otherwise prints `old => new`.
fn stat_name(f: &FileChange) -> Vec<u8> {
    match &f.source {
        // `pprint_rename()` quotes the pair itself; a plain name goes through
        // `quote_c_style()` in `fill_print_name()`.
        Some(source) => super::diff_pairs::pprint_rename(source, &f.path),
        None => super::diff_files::quoted_name_bytes(&f.path),
    }
}

/// Approximate display width. Paths are treated as UTF-8 and counted in `char`s,
/// which matches git for everything but wide and combining characters.
fn display_width(path: &[u8]) -> usize {
    String::from_utf8_lossy(path).chars().count()
}

// ---------------------------------------------------------------------------
// -p / --patch
// ---------------------------------------------------------------------------

/// Render one file-level change as a `diff --git` block.
fn emit_patch(repo: &gix::Repository, out: &mut Vec<u8>, f: &FileChange) -> Result<()> {
    // A rename names both sides in the header, and `fill_metainfo()` follows it with
    // the similarity index and the two `rename` lines.
    let old_path = f.source.as_deref().unwrap_or(&f.path);
    emit_git_header(out, old_path, &f.path);

    match (f.old_mode, f.new_mode) {
        (None, Some(new)) => writeln!(out, "new file mode {new:o}")?,
        (Some(old), None) => writeln!(out, "deleted file mode {old:o}")?,
        (Some(old), Some(new)) if old != new => {
            writeln!(out, "old mode {old:o}")?;
            writeln!(out, "new mode {new:o}")?;
        }
        _ => {}
    }
    if let Some(source) = &f.source {
        writeln!(out, "similarity index {}%", f.score)?;
        out.extend_from_slice(b"rename from ");
        out.extend_from_slice(source);
        out.push(b'\n');
        out.extend_from_slice(b"rename to ");
        out.extend_from_slice(&f.path);
        out.push(b'\n');
    }

    // A pure mode change (identical content) prints no index line and no hunks — and
    // so does a rename that moved the content unchanged.
    if f.mode_only {
        return Ok(());
    }

    let old_short = short_oid(repo, &f.old_id, f.old_mode.is_none() || f.old_is_sub)?;
    let new_short = short_oid(repo, &f.new_id, f.new_mode.is_none() || f.new_is_sub)?;
    match (f.old_mode, f.new_mode) {
        // The mode suffix is dropped when a mode change was already reported above.
        (Some(old), Some(new)) if old == new => writeln!(out, "index {old_short}..{new_short} {new:o}")?,
        _ => writeln!(out, "index {old_short}..{new_short}")?,
    }

    let old_path = f.old_mode.map(|_| old_path);
    let new_path = f.new_mode.map(|_| f.path.as_slice());
    if f.is_binary {
        emit_binary_line(out, old_path, new_path);
        return Ok(());
    }
    emit_body(out, old_path, new_path, &f.old_content, &f.new_content)
}

/// `diff --git a/<old> b/<new>` line, preserving raw path bytes. The two differ only
/// for a rename.
fn emit_git_header(out: &mut Vec<u8>, old_path: &[u8], new_path: &[u8]) {
    out.extend_from_slice(b"diff --git a/");
    out.extend_from_slice(old_path);
    out.extend_from_slice(b" b/");
    out.extend_from_slice(new_path);
    out.push(b'\n');
}

/// `Binary files <a> and <b> differ`, where a `None` side is `/dev/null`.
fn emit_binary_line(out: &mut Vec<u8>, old: Option<&[u8]>, new: Option<&[u8]>) {
    out.extend_from_slice(b"Binary files ");
    match old {
        Some(p) => {
            out.extend_from_slice(b"a/");
            out.extend_from_slice(p);
        }
        None => out.extend_from_slice(b"/dev/null"),
    }
    out.extend_from_slice(b" and ");
    match new {
        Some(p) => {
            out.extend_from_slice(b"b/");
            out.extend_from_slice(p);
        }
        None => out.extend_from_slice(b"/dev/null"),
    }
    out.extend_from_slice(b" differ\n");
}

/// Emit the `---`/`+++` file headers and hunks, but only when there is an actual
/// textual change (an empty-file add/delete produces no header lines, like git).
fn emit_body(
    out: &mut Vec<u8>,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
    old_content: &[u8],
    new_content: &[u8],
) -> Result<()> {
    let mut hunks: Vec<u8> = Vec::new();
    emit_text_hunks(&mut hunks, old_content, new_content)?;
    if hunks.is_empty() {
        return Ok(());
    }

    out.extend_from_slice(b"--- ");
    match old {
        Some(p) => {
            out.extend_from_slice(b"a/");
            out.extend_from_slice(p);
        }
        None => out.extend_from_slice(b"/dev/null"),
    }
    out.push(b'\n');

    out.extend_from_slice(b"+++ ");
    match new {
        Some(p) => {
            out.extend_from_slice(b"b/");
            out.extend_from_slice(p);
        }
        None => out.extend_from_slice(b"/dev/null"),
    }
    out.push(b'\n');

    out.extend_from_slice(&hunks);
    Ok(())
}

/// Compute the unified diff of two blobs into `out` using git's default settings.
fn emit_text_hunks(out: &mut Vec<u8>, old: &[u8], new: &[u8]) -> Result<()> {
    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    let before_lines: Vec<&[u8]> = input.before.iter().map(|&t| input.interner[t]).collect();
    let writer = HunkWriter { out, before_lines };
    UnifiedDiff::new(&diff, &input, writer, ContextSize::symmetrical(3)).consume()?;
    Ok(())
}

/// Writes each hunk in git's unified-diff style: `@@ -a +b @@ <func>` headers with
/// the git length-1 abbreviation, per-line prefixes, and the no-newline marker.
struct HunkWriter<'a> {
    out: &'a mut Vec<u8>,
    /// Pre-image lines, for resolving the function context of each hunk header.
    before_lines: Vec<&'a [u8]>,
}

impl<'a> HunkWriter<'a> {
    /// Find the nearest "function" line above the hunk's leading context, mirroring
    /// git's default (no `xfuncname`) heuristic: a line whose first byte is a letter,
    /// `_`, or `$`. Returns the trimmed line, or `None` if none is found.
    fn find_func(&self, before_hunk_start: u32) -> Option<&'a [u8]> {
        // 0-based index of the hunk's first shown line; scan strictly above it.
        let ctx_start = before_hunk_start.saturating_sub(1);
        let mut idx = ctx_start as i64 - 1;
        while idx >= 0 {
            let line = trim_end_ws(self.before_lines[idx as usize]);
            if let Some(&first) = line.first() {
                if first.is_ascii_alphabetic() || first == b'_' || first == b'$' {
                    return Some(line);
                }
            }
            idx -= 1;
        }
        None
    }
}

impl<'a> ConsumeHunk for HunkWriter<'a> {
    type Out = ();

    fn consume_hunk(&mut self, header: HunkHeader, lines: &[(DiffLineKind, &[u8])]) -> std::io::Result<()> {
        self.out.extend_from_slice(b"@@ -");
        write_range(self.out, header.before_hunk_start, header.before_hunk_len);
        self.out.extend_from_slice(b" +");
        write_range(self.out, header.after_hunk_start, header.after_hunk_len);
        self.out.extend_from_slice(b" @@");
        if let Some(func) = self.find_func(header.before_hunk_start) {
            self.out.push(b' ');
            self.out.extend_from_slice(func);
        }
        self.out.push(b'\n');

        for &(kind, content) in lines {
            self.out.push(match kind {
                DiffLineKind::Context => b' ',
                DiffLineKind::Add => b'+',
                DiffLineKind::Remove => b'-',
            });
            self.out.extend_from_slice(content);
            if !content.ends_with(b"\n") {
                self.out.push(b'\n');
                self.out.extend_from_slice(b"\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) {}
}

/// git omits the `,len` field when the hunk spans exactly one line, and points an
/// empty side at the line *before* the change rather than at a line that is not there.
fn write_range(out: &mut Vec<u8>, start: u32, len: u32) {
    match len {
        0 => {
            let _ = write!(out, "{},0", start.saturating_sub(1));
        }
        1 => {
            let _ = write!(out, "{start}");
        }
        _ => {
            let _ = write!(out, "{start},{len}");
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Abbreviated object id for the `index` and raw lines. Real objects are
/// disambiguated against the odb, as git's `diff_unique_abbrev` does; an absent
/// side is all zeros, and a submodule commit (which this odb does not have) is
/// plainly truncated.
fn short_oid(repo: &gix::Repository, id: &ObjectId, plain: bool) -> Result<String> {
    // git abbreviates the `index` line to `core.abbrev` (default auto), and pads
    // the all-zero/submodule side to that same width — never a hardcoded 7.
    let abbrev = crate::abbrev::configured_abbrev(repo, repo.object_hash().len_in_hex())
        .max(MINIMUM_ABBREV);
    if id.is_null() {
        return Ok("0".repeat(abbrev));
    }
    if plain {
        return Ok(id.to_hex_with_len(abbrev).to_string());
    }
    Ok(id.attach(repo).shorten()?.to_string())
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

/// Strip trailing newlines (`\n`/`\r`) — used to trim a commit message before indenting.
fn trim_trailing_newlines(mut s: &[u8]) -> &[u8] {
    while let Some(&last) = s.last() {
        if last == b'\n' || last == b'\r' {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

/// Strip trailing whitespace (git trims the function-context line this way).
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

// ---------------------------------------------------------------------------
// Dates (shared with `git log`; see log.rs for the same machinery)
// ---------------------------------------------------------------------------

/// The `log.date` / `--date=<mode>` output modes rendered byte-for-byte, plus
/// `relative`, measured against the current wall clock. The remaining zone- or
/// process-time-dependent modes (`human`, `local`) are rejected rather than faked.
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

/// Map a `log.date` / `--date=` value to a [`DateMode`]. `None` for a value git
/// accepts but renders time/zone-dependently (surfaced terse) or does not know.
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
                DateMode::IsoStrict => {
                    // git's `iso-strict` renders a zero (UTC) offset as `Z`, not
                    // `+00:00`; a non-zero offset uses the `±HH:MM` form.
                    let zone = if offset == 0 {
                        "Z".to_string()
                    } else {
                        format!("{sign}{oh:02}:{om:02}")
                    };
                    format!("{year}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}{zone}")
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

/// git's default (`DATE_NORMAL`) commit-time rendering: `Www Mmm D HH:MM:SS YYYY
/// +ZZZZ` in the commit's own timezone. The day is an unpadded decimal (git's
/// `%d`), matching a single-digit day to one space — unlike a `%e`-style pad.
fn format_git_date(seconds: i64, offset: i32) -> String {
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
/// month and day 1-based (Howard Hinnant's `civil_from_days`).
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

/// Current time in epoch seconds, for relative dates.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// git's `show_date_relative`: render `then` as "N units ago" relative to `now`,
/// with the same unit thresholds and rounding.
fn format_relative(then: i64, now: i64) -> String {
    if now < then {
        return "in the future".to_string();
    }
    let mut diff = (now - then) as u64;
    if diff < 90 {
        return unit_ago(diff, "second");
    }
    diff = (diff + 30) / 60; // minutes
    if diff < 90 {
        return unit_ago(diff, "minute");
    }
    diff = (diff + 30) / 60; // hours
    if diff < 36 {
        return unit_ago(diff, "hour");
    }
    diff = (diff + 12) / 24; // days
    if diff < 14 {
        return unit_ago(diff, "day");
    }
    if diff < 70 {
        return unit_ago((diff + 3) / 7, "week");
    }
    if diff < 365 {
        return unit_ago((diff + 15) / 30, "month");
    }
    if diff < 1825 {
        let totalmonths = diff * 12 * 10 / 365;
        let years = totalmonths / 120;
        let months = (totalmonths % 120) / 10;
        if months > 0 {
            return format!("{}, {} ago", unit(years, "year"), unit(months, "month"));
        }
        return unit_ago(years, "year");
    }
    unit_ago((diff + 183) / 365, "year")
}

/// `"N unit ago"` / `"N units ago"` with git's singular/plural rule.
fn unit_ago(n: u64, name: &str) -> String {
    format!("{} ago", unit(n, name))
}

/// `"1 unit"` or `"N units"`.
fn unit(n: u64, name: &str) -> String {
    if n == 1 {
        format!("1 {name}")
    } else {
        format!("{n} {name}s")
    }
}
