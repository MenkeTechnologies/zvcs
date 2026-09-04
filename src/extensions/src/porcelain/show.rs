use anyhow::{bail, Result};
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

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
use super::diff_color;
use super::diffstat::{self, StatWidths};
use super::line_log;
use super::log::{parse_date_mode, DateMode, DecorateStyle, Decorations, Mailmap, Pretty};

/// git's `MINIMUM_ABBREV`: the shortest id `--abbrev=<n>` may ask for, and the width
/// the all-zero side of an `index`/raw line is padded to when nothing longer is set.
const MINIMUM_ABBREV: usize = 4;

/// git's `DEFAULT_ABBREV`, the length a valueless `--abbrev` selects.
const DEFAULT_ABBREV: usize = 7;

/// `git show` — show one or more objects (commit, tree, blob, or annotated tag).
///
/// Implemented invocation forms:
///   * `git show [<commit>]`  → a commit header in the selected pretty format,
///     followed by the selected diff output. A non-merge diffs against its first
///     parent (a root commit against the empty tree); a merge takes whichever
///     shape `--diff-merges` selected, defaulting to the dense combined diff.
///   * `git show <blob>`      → the raw blob bytes.
///   * `git show <tree>`      → `tree <name-as-given>` then the top-level entry
///     names, directories suffixed with `/`.
///   * `git show <tag>`       → the annotated-tag header, then the object it points to.
///
/// Pretty formats: every format `git log` renders, rendered by `git log`'s own
/// code. `cmd_show` runs the same `cmd_log_init` and prints each record through
/// the same `show_log()`/`pretty_print_commit()` pair, so the format is parsed by
/// [`super::log::get_commit_format`] and the header is written by
/// [`super::log::EntryRenderer`] — `medium`, `short`, `full`, `fuller`, `raw`,
/// `oneline`, `reference`, the mail formats `email`/`mboxrd`, and
/// `--format=`/`--pretty=` with the placeholder set listed in `git log`'s
/// `check_format`. A placeholder neither command implements is rejected rather
/// than silently dropped, and it is rejected identically by both.
///
/// The record separator follows from that parse: `get_commit_format` also answers
/// `show_log()`'s `use_terminator`, so `oneline`, `reference` and `tformat:`/
/// `--format=` end each record with the terminator byte while every other format
/// puts it *between* records (log-tree.c:776-793, 915-919). Under `-z` that byte
/// is a NUL in both places.
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
/// detection, and the `\ No newline at end of file` marker.
///
/// Output is uncolored — the `git --no-color show` / non-tty case — except under
/// `--color-words[=<re>]` and `--word-diff=color`, the two spellings that set
/// `options->use_color = GIT_COLOR_ALWAYS` in `diff_opt_word_diff()`.
/// `log_tree_commit()` hands the header the same `o->use_color`, so those two paint
/// the whole record: the `commit <id>` line, the decorations, the patch body, the
/// diffstat graph and a merge's combined sections alike. `--color`/`--color=always`
/// stay refused — nothing but this family has been measured against stock — while
/// `--no-color`, `--color=never` and `--color=auto` are accepted and inert.
///
/// `--word-diff=plain` and `=porcelain`, which need no colour, are rendered.
///
/// A merge commit's diff is `log_tree_diff()`'s (log-tree.c:1131-1173), selected by
/// `--diff-merges=<mode>` and its shorthands `-m`, `-c`, `--cc`, `--dd` and
/// `--no-diff-merges`, and defaulted by `show_setup_revisions_tweak()`
/// (builtin/log.c:651-659) to `dense-combined` — or to `first-parent` under
/// `--first-parent`, which also upgrades an explicit `separate` to `first-parent`.
/// `off`/`none` prints the header alone; `separate` repeats the whole record once
/// per parent with `show_log()`'s ` (from <oid>)` insert; `combined` and
/// `dense-combined` print one `diff --combined` / `diff --cc` section set, with the
/// count formats measured against the first parent and printed ahead of the
/// combined raw block (`diff_tree_combined()`, combine-diff.c:1600-1610).
/// `--remerge-diff` / `--diff-merges=remerge` parses, and is refused where
/// `do_remerge_diff()` would run: this port has no merge engine to re-run, so a
/// request that reaches no merge behaves exactly as git's does and one that reaches
/// a merge says so instead of guessing. `--combined-all-paths` is refused for the
/// same reason — the shared combined engine prints one `--- a/<path>`, not one per
/// parent.
///
/// `--check` is `DIFF_FORMAT_CHECKDIFF`, which `diff_setup_done()` lets clear every
/// other output format: the record becomes its header and the whitespace report,
/// and `diff_result_code()`'s `02` bit lands in the exit status. `--exit-code` is
/// the `01` bit, and it makes `log_tree_diff()`'s `all_need_diff` true on its own,
/// so the change queue is built even under `-s`. Neither is set by a merge under a
/// combined mode, which is what `diff_tree_combined()` leaves alone.
///
/// `--line-prefix=<s>` is `diff_line_prefix()`, written in front of every emitted
/// line including the header — except a merge's combined `--name-only`/
/// `--name-status` records, which `show_raw_diff()` leaves unprefixed
/// (combine-diff.c:1244). It is refused beside `-z`: git prefixes each
/// NUL-terminated *record* rather than each NUL, which a whole-buffer pass cannot
/// reproduce for the three-field rename form.
///
/// `-S`/`-G` filtering takes `--pickaxe-all` (keep the whole queue once anything
/// matched) and `--pickaxe-regex` (promote `-S`'s literal to a regular expression).
/// It reaches a merge's combined sections too: `find_paths_generic()` runs
/// `diffcore_std()` against each parent and intersects what survives, so a path is
/// kept only where it hit against every parent.
/// `--no-rename-empty` turns off `record_if_better()`'s empty-blob pairing, so an
/// empty file that moved reports as a deletion plus an addition.
///
/// The diff formats `git diff` renders are shared: `--dirstat[=<params>]`,
/// `--dirstat-by-file`, `--cumulative`, `--compact-summary`, `--relative[=<path>]`
/// (narrowing every format, shortening only the writers `strip_prefix()` reaches),
/// `--diff-filter=<letters>` (which also clears `always_show_header`, so a commit
/// the filter empties prints nothing), `--output-indicator-{new,old,context}` and
/// `--ws-error-highlight=<kind>`.
///
/// Deviations, surfaced rather than faked:
///   * `--stat` is the shared [`super::diffstat`] port of `show_stats()`: the name
///     column is measured in display columns and the total width comes from
///     `term_columns()` (`$COLUMNS`, else 80 — there is no `TIOCGWINSZ` probe), with
///     `--stat-width`/`--stat=<w>`, `--stat-name-width`, `--stat-graph-width`,
///     `--stat-count` and the `diff.statNameWidth`/`diff.statGraphWidth` config all
///     honored.
///
/// Revision arguments accept the full walk grammar: plain names are shown directly
/// (deduplicated per commit, in argument order), while anything that excludes drives a
/// revision walk instead — `cmd_show` starts with `rev.no_walk = 1` and
/// `add_pending_object_with_path()` clears it as soon as a pending object carries
/// `UNINTERESTING`. That covers `^a`, the left side of a range (`a..b`), the merge
/// bases of a symmetric difference (`a...b`), the parents `a^!` and `a^-<n>` select,
/// and `--not`,
/// which flips the sense of every revision after it (and is undone by a second
/// `--not`). Under a walk the pending objects are peeled through their tag chain and
/// anything that is not a commit — a tree, a blob — contributes nothing, while `a^@`
/// stays a no-walk record of the parents themselves. All three marks are decoded by
/// [`crate::objname::parents_only`] *before* the revision parser sees the operand,
/// because they are `handle_revision_arg_1()`'s own grammar (revision.c:2178-2207)
/// rather than the parser's — `get_oid_1()` has no case for any of them. The walk itself is `git log`'s
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
/// `sorted` and `unsorted` are indistinguishable here. `handle_one_ref()` pends the
/// object each ref *names*, with no peeling (revision.c:1625-1637), so a ref on an
/// annotated tag reaches `cmd_show`'s `case OBJ_TAG:` and prints its `tag <name>`
/// block; only the walk, which peels in `prepare_revision_walk()`, sees through it.
/// Every flag not listed above is rejected explicitly.
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
    // `get_commit_format`'s second half: whether the format terminates each
    // record (`oneline`, `reference`, `tformat:`/`--format=`) or separates them
    // (every built-in header format and `format:`). `show_log()` reads it as
    // `rev->use_terminator` (log-tree.c:776, 915).
    let mut terminator = false;
    // `rev->pretty_given`, which is what decides whether notes show by default.
    let mut pretty_given = false;
    // `--[no-]encode-email-headers`; `None` leaves `format.encodeEmailHeaders`
    // (and its `default_encode_email_headers = 1`) in charge.
    let mut encode_email_headers: Option<bool> = None;
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
    // `--diff-merges=<mode>` and its shorthands `-m`/`-c`/`--cc`/`--dd`/
    // `--no-diff-merges` (diff-merges.c:119-151). `None` is git's
    // `revs->explicit_diff_merges == 0`, which is what lets
    // `show_setup_revisions_tweak()` pick a default; any spelling sets it, and
    // the last one wins.
    let mut diff_merges: Option<super::log::DiffMerges> = None;
    // `set_remerge_diff()`. The mode is recorded separately from
    // [`super::log::DiffMerges`] because this port has no remerge engine: the
    // flag parses, and a merge that would need one is refused where
    // `do_remerge_diff()` would run (log-tree.c:1134-1143) rather than at parse
    // time, so a request that reaches no merge behaves exactly as git's does.
    let mut remerge = false;
    // `--combined-all-paths` (`revs->combined_all_paths`). Only the arity rule
    // `diff_merges_setup_revs()` enforces (diff-merges.c:184-185) is reproduced;
    // the flag itself is refused below, because the shared combined patch engine
    // prints one `--- a/<path>` rather than one per parent.
    let mut combined_all_paths = false;
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
    // `--pickaxe-all` (`o->pickaxe_opts & DIFF_PICKAXE_ALL`) and `--pickaxe-regex`
    // (`DIFF_PICKAXE_REGEX`), the two knobs `diffcore_pickaxe()` reads beside the
    // needle itself: the first keeps the whole queue when any pair matched, the
    // second promotes `-S`'s literal to a regular expression.
    let mut pickaxe_all = false;
    let mut pickaxe_regex = false;
    // `--line-prefix=<s>` (`diff_line_prefix()`): the string `emit_line_0()` writes
    // in front of every emitted line, header included.
    let mut line_prefix: Vec<u8> = Vec::new();
    // `--exit-code` (`o->flags.exit_with_status`): `diff_result_code()` sets the
    // `01` bit of the exit status when the run found changes. It also makes
    // `log_tree_diff()`'s `all_need_diff` true on its own, so the diff is computed
    // even with no output format asking for one.
    let mut exit_code = false;
    // `--stat` width geometry; seeded from config after repo discovery, then any
    // explicit `--stat*` flag below wins (git precedence).
    let mut stat_widths = StatWidths::default();
    // The patch-shaping options, handed to the shared renderer.
    let mut patch_opts = super::diff::PatchOpts::default();
    // `--dirstat[=<params>]` / `--dirstat-by-file[=<params>]` / `--cumulative`
    // (`diff_opt_dirstat()`, diff.c), all of which also turn the format on.
    let mut dirstat = super::diff_files::DirStat::default();
    // `--compact-summary` (`diff_opt_compact_summary()`, diff.c), which also turns
    // `--stat` on; `--no-compact-summary` clears only the annotation.
    let mut compact_summary = false;
    // `--relative[=<path>]` / `--no-relative` (`diff_opt_relative()`): the prefix the
    // reported set is narrowed to, and which `strip_prefix()` then removes from the
    // patch, raw, name and stat writers — but not from `--summary` or `--dirstat`.
    let mut relative: Option<Option<String>> = None;
    let mut no_relative_given = false;
    // `--color-moved*` / `--word-diff*` / `--color-words`, resolved against
    // `diff.colorMoved` / `diff.colorMovedWS` / `diff.wordRegex` after discovery.
    let mut move_word = diff_color::MoveWordOpts::default();
    // The `GIT_COLOR_ALWAYS` the two color spellings of that family force.
    let mut move_word_color: Option<diff_color::ColorWhen> = None;
    // Set while a separated `--color-moved-ws` / `--word-diff-regex` waits for its
    // value, and likewise for `--output-indicator-*` and `--ws-error-highlight`.
    let mut pending_move_word: Option<String> = None;
    let mut pending_indicator: Option<&'static str> = None;
    let mut pending_ws_error_highlight = false;
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
    // Set while a separated `-I` / `--ignore-matching-lines` waits for its pattern.
    let mut pending_ignore_regex = false;
    // `--show-signature` / `--no-show-signature` (`rev_info.show_signature`).
    let mut show_signature = false;
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
    // `revs->max_count`: -1 (unlimited) in git; `None` here.
    let mut max_count: Option<usize> = None;
    // `-n <n>`: the count lives in the next argv slot.
    let mut pending_max_count = false;
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

    for (idx, a) in args.iter().enumerate() {
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
        // A short option that spends the *next* argv slot on its value, with no
        // slot left to spend: `get_arg()` (parse-options.c:59-60) — or, for `-n`,
        // `handle_revision_opt()`'s own `argc <= 1` check (revision.c) — refuses it
        // ahead of the option's own arm, so `git show -S` is a usage error and not
        // a search for the empty string.
        //
        // `--` in the value slot is not a value: `setup_revisions()` cuts the
        // option region at the separator (`argv[i] = NULL; argc = i;`,
        // `revision.c`) before it parses anything, which is why `git show -S --`
        // is the same refusal. `-L` is exempt because it is `builtin_log_options`'
        // own entry, read in stage 1 where the separator is still an ordinary
        // slot — and the table below declines it for exactly that reason.
        //
        // The two tables that decide which refusal and which status live in
        // [`super::blame::trailing_option_missing_value`]: `-S`, `-G`, `-I`, `-O`
        // and `-l` are parse-options' ``switch `<c>' requires a value`` at 129,
        // `-n` is `revision.c`'s `error: -n requires an argument` at 128.
        let value_slot_empty = idx + 1 == args.len() || args[idx + 1] == "--";
        if value_slot_empty && s.starts_with('-') && !s.starts_with("--") {
            if let Some(code) = super::blame::trailing_option_missing_value(s)? {
                return Ok(code);
            }
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
        if std::mem::take(&mut pending_max_count) {
            match s.parse::<usize>() {
                Ok(n) => max_count = Some(n),
                Err(_) => crate::git_fatal!("'{s}': not an integer"),
            }
            no_walk = false;
            continue;
        }
        if std::mem::take(&mut pending_line_range) {
            line_ranges.push(a.clone());
            continue;
        }
        if std::mem::take(&mut pending_ignore_regex) {
            match super::diff_pickaxe::compile_regex(a.as_bytes()) {
                Ok(re) => patch_opts.ignore_lines.push(super::diff_pickaxe::Needle::Regex(re)),
                Err(_) => {
                    eprintln!("error: invalid regex given to -I: '{a}'");
                    return Ok(ExitCode::from(129));
                }
            }
            continue;
        }
        if let Some(flag) = pending_move_word.take() {
            if let Some(Err(msg)) = move_word.parse_flag(&format!("{flag}={a}"), &mut move_word_color)
            {
                eprintln!("{msg}");
                return Ok(ExitCode::from(129));
            }
            continue;
        }
        if let Some(name) = pending_indicator.take() {
            if let Err(msg) = set_indicator(&mut patch_opts, name, a) {
                eprintln!("{msg}");
                return Ok(ExitCode::from(129));
            }
            continue;
        }
        if std::mem::take(&mut pending_ws_error_highlight) {
            if let Err(accepted) = diff_color::parse_ws_error_highlight(a) {
                eprintln!("error: unknown value after ws-error-highlight={}", &a[..accepted]);
                return Ok(ExitCode::from(129));
            }
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
            // `--check`: `DIFF_FORMAT_CHECKDIFF` (diff.c). Declared
            // `PARSE_OPT_NONEG`, so there is no `--no-check`.
            "--check" => formats.check = true,
            // `--exit-code` is an `OPT_BOOL` (diff.c:6256), so its negation exists
            // and the last spelling on the line wins.
            "--exit-code" => exit_code = true,
            "--no-exit-code" => exit_code = false,
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
                terminator = true;
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
            // The deprecated `--show-notes=<ref>` keeps the default tree beside the
            // ref it names; `--notes=<ref>` replaces it. `--standard-notes` puts it
            // back and `--no-standard-notes` takes it away without counting as a
            // `--notes` of its own.
            "--standard-notes" => {
                notes_opt.standard();
                notes_opt.given = true;
            }
            "--no-standard-notes" => notes_opt.no_standard(),
            a if a.starts_with("--show-notes=") => {
                notes_opt.enable_ref_show(&a["--show-notes=".len()..]);
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
            "--pickaxe-all" => pickaxe_all = true,
            "--pickaxe-regex" => pickaxe_regex = true,
            // `diff_merges_parse_opts()` (diff-merges.c:119-151): each spelling
            // selects one of `func_by_opt()`'s modes and raises
            // `revs->explicit_diff_merges`, so the last one on the line wins and
            // the `cmd_show` tweak below stops supplying a default.
            //
            // `-m` runs `set_to_default()` — `set_separate` unless
            // `diff.mergesDefault`/`log.diffMerges` moved it, which this port does
            // not read — and then clears `merges_need_diff`. That clearing cannot
            // matter here: `cmd_show` sets `rev.diff = 1` unconditionally
            // (builtin/log.c:686), so `log_tree_diff()`'s `all_need_diff` is
            // already true and the merge is diffed either way.
            "-m" => {
                diff_merges = Some(super::log::DiffMerges::Separate);
                remerge = false;
            }
            "-c" => {
                diff_merges = Some(super::log::DiffMerges::Combined);
                remerge = false;
            }
            "--cc" => {
                diff_merges = Some(super::log::DiffMerges::DenseCombined);
                remerge = false;
            }
            "--dd" => {
                diff_merges = Some(super::log::DiffMerges::FirstParent);
                remerge = false;
            }
            "--no-diff-merges" => {
                diff_merges = Some(super::log::DiffMerges::Off);
                remerge = false;
            }
            "--remerge-diff" => {
                diff_merges = Some(super::log::DiffMerges::Separate);
                remerge = true;
            }
            // `diff_tree_combined()` prints one `--- a/<path>` per parent under
            // `--combined-all-paths` (combine-diff.c), which the shared combined
            // patch engine does not carry; refused rather than dropped silently.
            // git's own arity rule comes first, so the message a bare one gets is
            // still git's.
            "--combined-all-paths" => combined_all_paths = true,
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
            // `revs->encode_email_headers` (revision.c:2526-2529): the last
            // spelling on the line wins over `format.encodeEmailHeaders`.
            "--encode-email-headers" => encode_email_headers = Some(true),
            "--no-encode-email-headers" => encode_email_headers = Some(false),
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
                    // wherever it appears with exit 128. `cmd_show` runs the same
                    // `cmd_log_init` as `cmd_log`, so the parser is `git log`'s
                    // `get_commit_format` — every format name and placeholder the
                    // one renders, the other renders identically.
                    match super::log::get_commit_format(None, spec)? {
                        Some((p, t)) => {
                            pretty = p;
                            terminator = t;
                            pretty_given = true;
                        }
                        None => return Ok(fatal(&format!("invalid --pretty format: {spec}\n"))),
                    }
                } else if let Some(v) = s.strip_prefix("--notes=") {
                    notes_opt.enable_ref(v);
                    notes_opt.given = true;
                } else if let Some(v) = s.strip_prefix("--show-notes=") {
                    notes_opt.enable_ref_show(v);
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
                    diffstat::parse_stat_geometry(&mut stat_widths, v);
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
                } else if s == "--ignore-cr-at-eol" {
                    patch_opts.ws = super::diff::Whitespace::IgnoreCrAtEol;
                } else if s == "--textconv" {
                    // `cmd_log_init_defaults()` raises `flags.allow_textconv`, so this
                    // only restores the default; `--no-textconv` is what matters.
                    patch_opts.allow_textconv = true;
                } else if s == "--no-textconv" {
                    patch_opts.allow_textconv = false;
                } else if s == "--ext-diff" {
                    // `cmd_log_init_defaults()` leaves `flags.allow_external` down, so
                    // `show` reaches an external driver only through this flag.
                    patch_opts.allow_external = true;
                } else if s == "--no-ext-diff" {
                    patch_opts.allow_external = false;
                } else if s == "--full-index" {
                    patch_opts.full_index = true;
                } else if s == "-a" || s == "--text" {
                    patch_opts.text = true;
                // Diff-algorithm selection. `cmd_show()` runs `setup_revisions()`, which
                // hands every unrecognised token to `diff_opt_parse()` (revision.c:2721),
                // so `show` takes the same four spellings `git diff` does.
                } else if s == "--minimal" {
                    patch_opts.algorithm = Some(gix::diff::blob::Algorithm::MyersMinimal);
                } else if s == "--patience" {
                    patch_opts.algorithm = Some(gix::diff::blob::Algorithm::Patience);
                } else if s == "--histogram" {
                    patch_opts.algorithm = Some(gix::diff::blob::Algorithm::Histogram);
                } else if let Some(v) = s.strip_prefix("--diff-algorithm=") {
                    match super::diff_optval::parse_algorithm_value(v) {
                        Some(alg) => patch_opts.algorithm = Some(alg),
                        None => crate::git_fatal!(
                            "option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\""
                        ),
                    }
                } else if s == "--indent-heuristic" {
                    patch_opts.indent_heuristic = true;
                } else if s == "--no-indent-heuristic" {
                    patch_opts.indent_heuristic = false;
                } else if s == "--ignore-blank-lines" {
                    patch_opts.blank_lines = true;
                // `-I<re>` / `--ignore-matching-lines=<re>` (`diff_opt_ignore_regex()`,
                // diff.c:5859): `regcomp`ed with `REG_EXTENDED | REG_NEWLINE` and pushed
                // onto `options->ignore_regex`, so repeats accumulate.
                } else if s == "-I" || s == "--ignore-matching-lines" {
                    pending_ignore_regex = true;
                } else if let Some(v) = s
                    .strip_prefix("--ignore-matching-lines=")
                    .or_else(|| if s.len() > 2 { s.strip_prefix("-I") } else { None })
                {
                    match super::diff_pickaxe::compile_regex(v.as_bytes()) {
                        Ok(re) => {
                            patch_opts.ignore_lines.push(super::diff_pickaxe::Needle::Regex(re));
                        }
                        Err(_) => {
                            eprintln!("error: invalid regex given to -I: '{v}'");
                            return Ok(ExitCode::from(129));
                        }
                    }
                } else if let Some(v) = s.strip_prefix("--inter-hunk-context=") {
                    match v.parse::<usize>() {
                        Ok(n) => patch_opts.inter_hunk_ctx = n,
                        Err(_) => crate::git_fatal!("invalid argument to --inter-hunk-context: {v}"),
                    }
                } else if s == "--binary" {
                    patch_opts.binary = true;
                // `--submodule[=<format>]`: a bare flag is `DIFF_SUBMODULE_LOG`
                // (diff.c:6269), an unknown value a usage error (129).
                } else if s == "--submodule" {
                    patch_opts.submodule_format = super::diff::SubmoduleFormat::Log;
                } else if let Some(v) = s.strip_prefix("--submodule=") {
                    match super::diff::parse_submodule_params(v) {
                        Some(f) => patch_opts.submodule_format = f,
                        None => {
                            eprintln!("fatal: bad --submodule argument: {v}");
                            return Ok(ExitCode::from(129));
                        }
                    }
                } else if s == "-D" || s == "--irreversible-delete" {
                    patch_opts.irreversible_delete = true;
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
                // `--rename-empty` / `--no-rename-empty` (`o->flags.rename_empty`,
                // `diff_setup()`'s default 1): whether `record_if_better()` may pair
                // an empty blob, i.e. whether an empty file that moved reports as
                // `R100` or as a deletion plus an addition.
                } else if s == "--rename-empty" {
                    patch_opts.rename_empty = true;
                } else if s == "--no-rename-empty" {
                    patch_opts.rename_empty = false;
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
                // `--max-count=<n>` / `-<digit>` / `-n<n>` / `-n <n>`. Each of these
                // clears `revs->no_walk` alongside setting `revs->max_count`
                // (revision.c:2345-2346, 2366-2368, 2370-2374, 2376-2378), so
                // `cmd_show` takes its `if (!rev.no_walk)` branch and runs
                // `cmd_log_walk` (builtin/log.c:694-699) rather than its pending
                // loop — `git show -2` is a two-commit walk, not one commit.
                } else if let Some(v) = s
                    .strip_prefix("--max-count=")
                    .or_else(|| s.strip_prefix("-n").filter(|v| !v.is_empty()))
                    .or_else(|| {
                        s.strip_prefix('-')
                            .filter(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()))
                    })
                {
                    match v.parse::<usize>() {
                        Ok(n) => max_count = Some(n),
                        // `parse_count()` (revision.c) dies on a non-number.
                        Err(_) => crate::git_fatal!("'{v}': not an integer"),
                    }
                    no_walk = false;
                } else if s == "-n" {
                    // `-n` takes the next argv slot (revision.c:2370-2374).
                    pending_max_count = true;
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
                } else if s == "--show-signature" {
                    show_signature = true;
                } else if s == "--no-show-signature" {
                    show_signature = false;
                } else if let Some(v) = s.strip_prefix("--diff-filter=") {
                    patch_opts
                        .diff_filter
                        .get_or_insert_with(Vec::new)
                        .extend_from_slice(v.as_bytes());
                } else if s == "--relative" {
                    // The repository is not open yet, so the cwd prefix is resolved
                    // after discovery; `Some(None)` records "the valueless form".
                    relative = Some(None);
                    no_relative_given = false;
                } else if s == "--no-relative" {
                    relative = None;
                    no_relative_given = true;
                } else if let Some(v) = s.strip_prefix("--relative=") {
                    // git stores the prefix with a trailing slash so a plain prefix
                    // match cannot cross a name boundary.
                    let mut p = v.to_string();
                    if !p.is_empty() && !p.ends_with('/') {
                        p.push('/');
                    }
                    relative = Some(Some(p));
                    no_relative_given = false;
                } else if let Some(v) = s.strip_prefix("--line-prefix=") {
                    line_prefix = v.as_bytes().to_vec();
                } else if let Some(v) = s.strip_prefix("--diff-merges=") {
                    match super::log::DiffMerges::parse(v) {
                        Some(m) => {
                            diff_merges = Some(m);
                            remerge = false;
                        }
                        // `func_by_opt()` (diff-merges.c:82-83) does map
                        // `r`/`remerge` onto `set_remerge_diff()`, so calling it
                        // invalid would claim git rejects a value it accepts.
                        None if matches!(v, "r" | "remerge") => {
                            diff_merges = Some(super::log::DiffMerges::Separate);
                            remerge = true;
                        }
                        None => {
                            // `set_diff_merges()`'s `die()` (diff-merges.c:94).
                            return Ok(fatal(&format!(
                                "invalid value for '--diff-merges': '{v}'\n"
                            )));
                        }
                    }
                } else if s == "--compact-summary" {
                    compact_summary = true;
                    formats.stat = true;
                } else if s == "--no-compact-summary" {
                    compact_summary = false;
                } else if s == "--dirstat" {
                    formats.dirstat = true;
                } else if s == "--dirstat-by-file" {
                    formats.dirstat = true;
                    dirstat.by_file = true;
                } else if s == "--cumulative" {
                    formats.dirstat = true;
                    dirstat.cumulative = true;
                } else if s.starts_with("--dirstat=") || s.starts_with("--dirstat-by-file=") {
                    let by_file = s.starts_with("--dirstat-by-file=");
                    let params = s.split_once('=').map(|(_, v)| v).unwrap_or_default();
                    let errors = super::diff_files::parse_dirstat_params(params, &mut dirstat);
                    if !errors.is_empty() {
                        // `parse_dirstat_opt()`'s `die()`, carrying the accumulated text.
                        eprint!(
                            "fatal: Failed to parse --dirstat/-X option parameter:\n{errors}\n"
                        );
                        return Ok(ExitCode::from(128));
                    }
                    if by_file {
                        dirstat.by_file = true;
                    }
                    formats.dirstat = true;
                } else if super::diff::history_noop_diff_option(s) {
                    // Accepted and inert; see the list's own documentation for why each
                    // entry cannot change a byte this command prints.
                // `--color-moved*` / `--word-diff*` / `--color-words`: the family that
                // re-emits the assembled patch instead of changing how it is built.
                // The two color spellings set `options->use_color = GIT_COLOR_ALWAYS`
                // (`diff_opt_word_diff()`), and this module has no colored output path
                // at all, so they stay refused rather than silently dropping the ANSI
                // stock would emit.
                //
                // `--color-moved-ws` and `--word-diff-regex` are declared without
                // `PARSE_OPT_OPTARG`, so a bare one takes the next argv entry.
                } else if diff_color::needs_separate_value(s) {
                    pending_move_word = Some(s.to_string());
                } else if let Some(res) = move_word.parse_flag(s, &mut move_word_color) {
                    if let Err(msg) = res {
                        eprintln!("{msg}");
                        return Ok(ExitCode::from(129));
                    }
                // `--output-indicator-new`/`-old`/`-context=<char>` (`diff_opt_char()`,
                // diff.c:5593): one byte replaces the sign a hunk line carries.
                } else if let Some(name) = super::log::indicator_name(s) {
                    match s.split_once('=') {
                        Some((_, v)) => {
                            if let Err(msg) = set_indicator(&mut patch_opts, name, v) {
                                eprintln!("{msg}");
                                return Ok(ExitCode::from(129));
                            }
                        }
                        None => pending_indicator = Some(name),
                    }
                // `--ws-error-highlight=<kind>`. Nothing is painted with color off, but
                // the value is validated the way `diff_opt_ws_error_highlight()` does.
                } else if s == "--ws-error-highlight" {
                    pending_ws_error_highlight = true;
                } else if let Some(v) = s.strip_prefix("--ws-error-highlight=") {
                    if let Err(accepted) = diff_color::parse_ws_error_highlight(v) {
                        eprintln!("error: unknown value after ws-error-highlight={}", &v[..accepted]);
                        return Ok(ExitCode::from(129));
                    }
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
    // `diff_merges_setup_revs()`'s only die (diff-merges.c:184-185) — checked
    // after the whole line is parsed, so `--combined-all-paths -c` passes it just
    // as `-c --combined-all-paths` does.
    if !line_prefix.is_empty() && z {
        bail!("unsupported option --line-prefix with -z");
    }
    if combined_all_paths {
        if !matches!(
            diff_merges,
            Some(super::log::DiffMerges::Combined) | Some(super::log::DiffMerges::DenseCombined)
        ) {
            return Ok(fatal("--combined-all-paths makes no sense without -c or --cc\n"));
        }
        bail!("unsupported option --combined-all-paths");
    }
    // `show_setup_revisions_tweak()` (builtin/log.c:651-659), the whole of what
    // makes `git show` differ from `git log` here:
    //
    // ```c
    // if (rev->first_parent_only)
    //         diff_merges_default_to_first_parent(rev);
    // else
    //         diff_merges_default_to_dense_combined(rev);
    // ```
    //
    // `diff_merges_default_to_dense_combined()` only fires when nothing explicit
    // was given, so a lone `git show <merge>` is `--cc`.
    // `diff_merges_default_to_first_parent()` (diff-merges.c:158-164) is the
    // two-step one: it turns `separate_merges` on when nothing explicit was given,
    // and *then* upgrades `separate` to `first-parent` — so
    // `--diff-merges=separate --first-parent` is first-parent, while
    // `--first-parent --diff-merges=combined` stays combined.
    let merges = match (diff_merges, first_parent) {
        (Some(super::log::DiffMerges::Separate), true) => super::log::DiffMerges::FirstParent,
        (Some(m), _) => m,
        (None, true) => super::log::DiffMerges::FirstParent,
        (None, false) => super::log::DiffMerges::DenseCombined,
    };
    // `-L` (`rev->line_level_traverse`), rejected against the formats and the
    // pathspec exactly as `cmd_log_init_finish` rejects them.
    let line_level = !line_ranges.is_empty();
    if line_level {
        // git's allowed set is PATCH / NO_OUTPUT / RAW / NAME / NAME_STATUS /
        // SUMMARY; `DIFF_FORMAT_CHECKDIFF` and the count formats are not in it.
        if formats.stat || formats.check {
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
    for flag in [
        pending_move_word.as_deref(),
        pending_indicator,
        pending_ws_error_highlight.then_some("--ws-error-highlight"),
    ]
    .into_iter()
    .flatten()
    {
        eprintln!("error: {}", diff_color::missing_value(flag));
        return Ok(ExitCode::from(129));
    }
    if let Some(include) = pending_decorate_refs {
        let name = if include {
            "decorate-refs"
        } else {
            "decorate-refs-exclude"
        };
        eprintln!("error: option `{name}' requires a value");
        return Ok(ExitCode::from(129));
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

    let mut repo = crate::setup::discover()?;
    let hex_len = repo.object_hash().len_in_hex();

    // `--word-diff`/`--color-moved` layered over `diff.wordRegex` / `diff.colorMoved`.
    // The palette stays disabled: this module has no colored output path, and both
    // spellings that would force color on are refused above, so the move detector
    // (which git only runs with `o->emitted_symbols` allocated, i.e. with color on)
    // is inert here exactly as it is in stock.
    // `--relative[=<path>]`, plus the `diff.relative` config that seeds the same
    // flag (`options->flags.relative_name = diff_relative`, diff.c:5155). An explicit
    // `--no-relative` beats the config.
    patch_opts.relative = match (&relative, repo.config_snapshot().boolean("diff.relative")) {
        (Some(Some(p)), _) => Some(p.clone()),
        (Some(None), _) => Some(super::diff::cwd_prefix(&repo)),
        (None, Some(true)) if !no_relative_given => Some(super::diff::cwd_prefix(&repo)),
        _ => None,
    };

    // `diff_opt_word_diff()` sets `options->use_color = GIT_COLOR_ALWAYS` for the
    // two color spellings (`--color-words[=<re>]`, `--word-diff=color`), and
    // `log_tree_commit()` hands the header the same `o->use_color` — so one of them
    // anywhere on the line paints the whole record, exactly as it does in `git log`.
    // No other spelling turns color on here: `--color`/`--color=always` are still
    // refused, since nothing but this family has been measured against stock.
    let want_color = move_word_color == Some(diff_color::ColorWhen::Always);
    patch_opts.extra = match move_word.resolve(&repo) {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("{msg}");
            return Ok(ExitCode::from(128));
        }
    };
    // The palette the re-emit pass paints with, resolved from `color.diff`/`color.ui`
    // exactly as `git log` resolves it — the empty table when this run is not
    // coloring, which is every run but the `--color-words` family's.
    patch_opts.colors = diff_color::DiffColors::resolve(&repo, want_color);

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

    // `if (w.source) rev->show_source = 1` (builtin/log.c): `git show` runs the
    // same `cmd_log_init()`, so a `%S` in the user format names the argument the
    // commit was reached from without `--source` being typed.
    let source_mode = source_mode || super::log::pretty_uses_source(&pretty);

    // The commit→refs map for `--decorate`, filtered exactly as `git log` filters
    // it; skipped entirely when no decorations will be shown so the ref scan costs
    // nothing on a plain `git show`.
    // `%d`/`%D` need the map too, whatever `--decorate` says.
    let decorations = if decorate == DecorateStyle::Off
        && !super::log::pretty_uses_decoration(&pretty)
    {
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
    // `%aN`/`%aE`/`%cN`/`%cE` resolve through the mailmap whether or not the header
    // formats do, so a format that names one loads it even under `--no-use-mailmap`
    // — `format_person_part()` reads `pp->mailmap` unconditionally.
    let format_maps_identities = match &pretty {
        Pretty::User(f) => super::log::format_names_mapped_identity(f),
        _ => false,
    };
    let mailmap = (use_mailmap || format_maps_identities).then(|| Mailmap::load(&repo));

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
            // `handle_one_ref()` (revision.c:1625-1637) pends `get_reference()`'s
            // object — the one the ref *names*, with no peeling at all — while a
            // walk peels in `prepare_revision_walk()`. `cmd_show` runs its pending
            // loop directly under `no_walk`, so an annotated tag reaches
            // `case OBJ_TAG:` (builtin/log.c:711-731) and prints its own
            // `tag <name>` block before the commit it points at. That is why
            // `direct` and `peeled` are tracked apart here.
            let mut pend = |direct: ObjectId, peeled: ObjectId, name: &str| {
                if sel.negated {
                    *no_walk = false;
                    walk_hidden.push(peeled);
                    return;
                }
                plain.push((name.to_string(), direct));
                walk_tips.push(peeled);
                walk_tip_sources.push(name.to_string());
            };
            for reference in repo.references()?.all()? {
                let Ok(reference) = reference else { continue };
                let full = reference.name().as_bstr().to_string();
                let Some(name) = sel.selects(&full) else { continue };
                let direct = reference.target().id().to_owned();
                let Ok(id) = reference.into_fully_peeled_id() else { continue };
                let oid = id.detach();
                if !repo.find_object(oid).is_ok_and(|o| o.kind == Kind::Commit) {
                    continue;
                }
                pend(direct, oid, name);
            }
            if sel.head && !sel.excluded("HEAD") {
                if let Some(id) = repo.head().ok().and_then(|mut h| h.try_peel_to_id().ok().flatten())
                {
                    pend(id.detach(), id.detach(), "HEAD");
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
        // Both of `get_oid_basic()`'s warnings, once per endpoint, with the
        // short-circuit `handle_dotdot_1()`'s `||` gives a range. The ambiguity
        // half is skipped for what `--stdin` supplied.
        super::log::warn_operand(&repo, spec, at < argv_specs);
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
        // `handle_revision_arg_1()`'s parent-mark block (revision.c:2178-2207),
        // decoded before the revision parser rather than after it. It has to be:
        // these marks are `handle_revision_arg_1()`'s own grammar, `get_oid_1()`
        // has no case for any of them, and gitoxide's `rev_parse()` has no `^-<n>`
        // variant at all — so `git show <merge>^-` came back as the merge alone,
        // at exit 0, where stock prints the merge and the parent the mark excluded.
        let mut spec: &str = *spec;
        match crate::objname::parents_only(spec) {
            crate::objname::ParentsOnly::Absent => {}
            // `strtol_i()` refused the `<n>`: `ret = -1`, so `add_parents_only()`
            // is never reached and the operand is diagnosed as written.
            crate::objname::ParentsOnly::BadParent => {
                return Ok(bad_revision(&repo, spec, seen_dashdash))
            }
            crate::objname::ParentsOnly::Mark { base, nth, replaces } => {
                // `^@` keeps `flags`; `^!` and `^-<n>` pass
                // `flags ^ (UNINTERESTING | BOTTOM)`.
                let sense = if replaces { negated } else { !negated };
                let mut queued: Vec<(String, ObjectId, bool)> = Vec::new();
                let mut queue = |name: &str, id: ObjectId, not: bool| {
                    queued.push((name.to_string(), id, not));
                };
                match crate::objname::add_parents_only(&repo, base, sense, nth, &mut queue) {
                    // `get_reference()`'s `die(_("bad object %s"), name)`, naming
                    // the base with its leading `^` already stripped.
                    crate::objname::Parents::BadObject => {
                        let name = crate::objname::uninteresting_mark(base).0;
                        eprintln!("fatal: bad object {name}");
                        return Ok(ExitCode::from(128));
                    }
                    // `return 0` leaves `arg` alone, and an operand that still
                    // carries a mark cannot resolve — `get_oid_1()` has no case
                    // for it — so this is the bad-revision fatal.
                    crate::objname::Parents::None => {
                        return Ok(bad_revision(&repo, spec, seen_dashdash))
                    }
                    crate::objname::Parents::Queued => {
                        parsed_commits.extend(super::log::navigation_path(&repo, base));
                        // `no_walk` is *not* touched here: `add_pending_object()`
                        // clears it at the argument's own position, which the
                        // scan in the option loop already reproduced — so a later
                        // `--no-walk` puts it back, and `git show <merge>^-1
                        // --no-walk` prints the merge alone.
                        for (name, id, not) in queued {
                            plain.push((name.clone(), id));
                            if not {
                                walk_hidden.push(id);
                            } else {
                                walk_tips.push(id);
                                walk_tip_sources.push(name);
                            }
                        }
                        // `if (add_parents_only(…)) { ret = 0; goto out; }` — `^@`
                        // claimed the operand and never pends the commit itself.
                        if replaces {
                            continue;
                        }
                        // `arg = arg_minus_excl;` / `arg = arg_minus_dash;`. The
                        // base may still carry its own leading `^`, which the
                        // ordinary resolution below strips a *second* time.
                        spec = base;
                    }
                }
            }
        }
        // `get_oid_basic()` reads a `<ref>@{…}` operand through `repo_dwim_log()`
        // and `read_ref_at()` (`object-name.c:742-789`) rather than through the
        // revspec grammar, and the two do not agree: gitoxide hands back the
        // selected entry's raw *new* id, which is the null id for the record
        // written by a `git branch -m` round trip, where `read_ref_at()` answers
        // with the ref's current value. See [`crate::objname::reflog_oid`].
        // The test is on the *reduced* name: `HEAD@{<n>}^{commit}` reaches
        // `get_oid_basic()` as `HEAD@{<n>}`, and the peel then works on the object
        // the reader answered with. See [`crate::objname::reflog_spec_oid`].
        let reflog = crate::objname::resolves_through_reflog(spec)
            .then(|| crate::objname::reflog_spec_oid(&repo, spec))
            .flatten()
            .map(RevSpec::Include);
        let parsed = match reflog {
            Some(parsed) => parsed,
            None => match repo.rev_parse(BStr::new(spec)) {
                Ok(p) => p.detach(),
                Err(_) => return Ok(bad_revision(&repo, spec, seen_dashdash)),
            },
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
                // `add_parents_only()` is handed `arg_minus_excl` — the argument
                // with its `^!` cut off — and `get_reference()` records *that* as
                // the pending object's name, so `--source` prints `HEAD`, not
                // `HEAD^!` (revision.c:2186-2191).
                let name = parents_only_name(spec, "^!");
                if negated {
                    for p in parents {
                        plain.push((name.to_string(), p));
                        walk_tips.push(p);
                        walk_tip_sources.push(name.to_string());
                    }
                    plain.push((name.to_string(), id));
                    walk_hidden.push(id);
                } else {
                    for p in &parents {
                        plain.push((name.to_string(), *p));
                    }
                    walk_hidden.extend(parents);
                    plain.push((name.to_string(), id));
                    walk_tips.push(id);
                    walk_tip_sources.push(name.to_string());
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
                // Same rule as `^!`: the name is `arg_minus_at`, the argument with
                // its `^@` cut off (revision.c:2178-2184).
                let name = parents_only_name(spec, "^@");
                // `handle_revision_arg_1()` strips the leading `^` into `flags_exclude`
                // *before* it looks at the `^@` suffix, so `^<rev>^@` pends the parents
                // UNINTERESTING. gitoxide's `rev_parse()` folds that `^` into its
                // `Exclude…` variants but not into `IncludeOnlyParents`, which comes
                // back as though the caret were not there — so it is read from the
                // token here rather than trusted to the variant.
                let negated = negated ^ spec.starts_with('^');
                for p in parents_of(&repo, id)? {
                    if negated {
                        plain.push((name.to_string(), p));
                        walk_hidden.push(p);
                    } else {
                        plain.push((name.to_string(), p));
                        walk_tips.push(p);
                        walk_tip_sources.push(name.to_string());
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
            super::log::topo_sort(nodes, order == super::log::Order::Date, first_parent)
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
    // `-S`'s needle is a literal unless `--pickaxe-regex` promoted it, which is the
    // `DIFF_PICKAXE_REGEX` branch of `diffcore_pickaxe()`'s `has_changes` —
    // `pickaxe_match()` then counts regex matches where it counted substrings.
    // `diff_setup_done()` (diff.c): `DIFF_PICKAXE_REGEX` is `-S`'s modifier, and git
    // names the pairing explicitly rather than ignoring it.
    if pickaxe_regex && pickaxe_g.is_some() {
        return Ok(fatal(
            "options '-G' and '--pickaxe-regex' cannot be used together, \
             use '--pickaxe-regex' with '-S'\n",
        ));
    }
    let pickaxe = Pickaxe {
        s: match (&pickaxe_s, pickaxe_regex) {
            (None, _) => None,
            (Some(needle), false) => {
                Some(super::diff_pickaxe::Needle::Literal(needle.as_bytes().to_vec()))
            }
            (Some(needle), true) => match super::diff_pickaxe::compile_regex(needle.as_bytes()) {
                Ok(re) => Some(super::diff_pickaxe::Needle::Regex(re)),
                Err(msg) => {
                    eprintln!("fatal: invalid regex: {msg}");
                    return Ok(ExitCode::from(128));
                }
            },
        },
        g: match &pickaxe_g {
            Some(p) => Some(crate::revfilter::build_regex(
                p,
                crate::revfilter::Dialect::Basic,
                false,
                crate::revfilter::Origin::CommandLine,
            )?),
            None => None,
        },
        all: pickaxe_all,
    };

    let mut out: Vec<u8> = Vec::new();
    // git marks each commit it prints as SHOWN, so a commit named twice (or reached
    // twice by a walk) is printed once. Blobs, trees, and tags are not deduplicated.
    let mut shown: Vec<ObjectId> = Vec::new();
    // git's `rev_info.shown_one`, which drives the inter-record separator.
    let mut shown_one = false;
    if !notes_opt.given && (!pretty_given || matches!(pretty, Pretty::User(_))) {
        notes_opt.show_only();
    }
    let notes_trees = super::notes::load_display(&repo, &notes_opt)?;
    let (cfg_subject_prefix, cfg_encode_email_headers) = super::log::email_config(&repo);
    let renderer = super::log::EntryRenderer::with_color(&repo, want_color);
    let rename_warn = std::cell::RefCell::new(RenameWarnState::default());
    let diff_status = std::cell::Cell::new((false, false));
    let remerge_hit = std::cell::Cell::new(false);
    let no_prefix: std::cell::RefCell<Vec<(usize, usize)>> = std::cell::RefCell::new(Vec::new());
    let disp = DisplayOpts {
        show_signature,
        notes: &notes_trees,
        notes_shown: notes_opt.show,
        abbrev_commit,
        date_mode,
        show_root,
        merges,
        remerge,
        remerge_hit: &remerge_hit,
        no_prefix: &no_prefix,
        exit_code,
        status: &diff_status,
        stat: stat_widths,
        compact_summary,
        relative: patch_opts.relative.clone().unwrap_or_default(),
        dirstat,
        patch: patch_opts.clone(),
        decorate,
        decorations: decorations.as_ref(),
        mailmap: use_mailmap.then(|| mailmap.as_ref()).flatten(),
        identity_mailmap: mailmap.as_ref(),
        terminator,
        renderer: &renderer,
        rename_warn: &rename_warn,
        z,
        expand_tabs,
        email: super::log::EmailStyle {
            subject_prefix: &cfg_subject_prefix,
            encode_headers: encode_email_headers.unwrap_or(cfg_encode_email_headers),
        },
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
            let nodes = super::log::topo_sort(nodes, false, first_parent);
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
        // `--max-count`: `get_revision_internal()` stops handing commits back once the
        // counter runs out, and `--reverse` reverses what survived that limit rather
        // than limiting the reversed stream (revision.c:4683-4692).
        let mut nodes = walked;
        if let Some(n) = max_count {
            nodes.truncate(n);
        }
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

    // Persist whatever abbreviations the header renderer computed, as `git log`
    // does at the end of its walk.
    drop(disp);
    renderer.finish();

    // `--line-prefix`: `emit_line_0()` writes `diff_line_prefix(o)` in front of
    // every emitted line, and for a history verb that includes the header
    // `show_log()` wrote — measured against stock 2.55.0, `git show
    // --line-prefix='>>'` prefixes the `commit`/`Author:`/message lines too.
    //
    // The `-z` formats are the one shape this whole-buffer pass cannot reproduce:
    // git prefixes each NUL-terminated *record*, not each NUL, so a `--numstat -z`
    // rename (`<counts>\0<from>\0<to>\0`) carries one prefix where splitting on
    // NUL would write three. That combination is refused rather than approximated.
    let out = apply_line_prefix_except(out, &line_prefix, &no_prefix.borrow());
    // A merge reached under `--remerge-diff`: nothing this run printed is what git
    // would have printed, so the buffered records are dropped for the fatal.
    if remerge_hit.get() {
        eprintln!("fatal: --diff-merges=remerge is not supported by this build");
        return Ok(ExitCode::from(128));
    }
    // `diff_result_code()` (diff.c): `01` when `--exit-code` saw changes, `02` when
    // `--check` found a whitespace error. The two are or-ed, but they never both
    // fire: `DIFF_FORMAT_CHECKDIFF` is the only format under `--check`, and it
    // reports through `check_failed` alone.
    let (has_changes, check_failed) = diff_status.get();
    let result_code = u8::from(exit_code && has_changes) | (u8::from(check_failed) << 1);
    let mut stdout = std::io::stdout().lock();
    let rc = match stdout.write_all(&out).and_then(|()| stdout.flush()) {
        Ok(()) => Ok(ExitCode::from(result_code)),
        // A downstream `| head` closing the pipe is not an error; git leaves by
        // way of SIGPIPE rather than returning a status of its own.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
            crate::sigpipe::exit_broken_pipe()
        }
        Err(e) => Err(e.into()),
    };
    // `diff_result_code()` reports the rename limit the run would have needed, and
    // `diff_warn_rename_limit()` opens with `fflush(stdout)` — so the warnings land
    // after everything the command printed (diff.c:7038-7040, 7546-7548).
    let warns = rename_warn.borrow();
    if needs_walk {
        // One `cmd_log_walk()` for the whole walk, so one report: whatever the last
        // commit's rename pass left in `rev.diffopt`.
        warns.current.emit("diff.renameLimit");
    } else {
        for w in &warns.per_commit {
            w.emit("diff.renameLimit");
        }
    }
    rc
}

/// The name `add_parents_only()` pends a `<rev>^@` / `<rev>^!` argument under:
/// the argument with its mark cut off (`arg_minus_at` / `arg_minus_excl`,
/// revision.c:2178-2191) and, since `add_parents_only()` steps past a leading
/// `^` before calling `get_reference()` (revision.c:1898-1901), without that
/// either. `--source` prints this name, not the argument as written.
fn parents_only_name<'a>(spec: &'a str, mark: &str) -> &'a str {
    let base = spec.strip_suffix(mark).unwrap_or(spec);
    base.strip_prefix('^').unwrap_or(base)
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
/// `seen_dashdash` is `setup_revisions()`'s: once a `--` has been seen anywhere on
/// the line the operand can no longer be a pathspec, so
/// `if (seen_dashdash || *arg == '^') die(_("bad revision '%s'"), arg);`
/// (revision.c:3035-3036) prints the one-line form without the pathspec advice.
fn bad_revision(repo: &gix::Repository, spec: &str, seen_dashdash: bool) -> ExitCode {
    eprint!("{}", super::log::bad_revision_message_in_gated(repo, spec, seen_dashdash));
    ExitCode::from(128)
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
    /// `--dirstat`/`--dirstat-by-file`/`--cumulative`: `DIFF_FORMAT_DIRSTAT`.
    dirstat: bool,
    patch: bool,
    /// `--check`: `DIFF_FORMAT_CHECKDIFF`, one of the four mutually exclusive
    /// formats. `diff_setup_done()` lets it clear every other format bit, so a
    /// commit under it prints its whitespace report and nothing else.
    check: bool,
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
    /// `--check`: `diff_flush_checkdiff()` in place of every other format.
    Check,
    /// Any combination of the block formats, rendered in git's fixed order.
    Blocks {
        raw: bool,
        numstat: bool,
        stat: bool,
        shortstat: bool,
        dirstat: bool,
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
        self.check
            || self.no_output
            || self.name_only
            || self.name_status
            || self.raw
            || self.stat
            || self.numstat
            || self.shortstat
            || self.summary
            || self.dirstat
            || self.patch
    }

    /// The block formats, i.e. everything `--name-only`/`--name-status` overrides.
    fn any_block(self) -> bool {
        self.raw
            || self.stat
            || self.numstat
            || self.shortstat
            || self.summary
            || self.dirstat
            || self.patch
    }

    /// Apply git's precedence: `-s` suppresses output only when it is the sole
    /// format; `--name-only` beats raw/stat/patch; naming both `-s` and
    /// `--name-only` with no third format is an error.
    fn resolve(mut self) -> Result<Selection, FormatConflict> {
        if !self.any_set() {
            self.patch = true;
        }
        let names = self.name_only || self.name_status;
        // `diff_setup_done()` (diff.c): `HAS_MULTI_BITS(output_format & (NAME |
        // NAME_STATUS | CHECKDIFF | NO_OUTPUT))` dies. `-s` *assigns* NO_OUTPUT
        // rather than or-ing it, which is why `--check -s` is fine (the CHECKDIFF
        // bit is gone by then) while `-s --check` is the fatal.
        if usize::from(self.no_output && !self.any_block())
            + usize::from(self.name_only)
            + usize::from(self.name_status)
            + usize::from(self.check)
            > 1
        {
            return Err(FormatConflict);
        }
        if self.no_output && !names && !self.check && !self.any_block() {
            return Ok(Selection::Disabled);
        }
        // The same `diff_setup_done()` clearing: CHECKDIFF wins over every block
        // format, so `--check --stat` prints only the whitespace report.
        if self.check {
            return Ok(Selection::Check);
        }
        if names {
            return Ok(Selection::Names { status: self.name_status });
        }
        Ok(Selection::Blocks {
            raw: self.raw,
            numstat: self.numstat,
            stat: self.stat,
            shortstat: self.shortstat,
            dirstat: self.dirstat,
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
/// `rev.diffopt`'s two rename-limit fields across a whole `git show` run.
///
/// `cmd_show` keeps one `rev_info` for every object it prints and sets
/// `rev.diffopt.no_free = 1`, so the fields carry over from one pending commit to
/// the next: a commit whose own rename pass never reached
/// `too_many_rename_candidates()` reports whatever the commit before it left there.
#[derive(Default)]
struct RenameWarnState {
    /// The live `rev.diffopt` fields.
    current: super::diffcore_rename::Warnings,
    /// Their value at the end of each rendered commit — one `diff_result_code()`
    /// call apiece under `no_walk`.
    per_commit: Vec<super::diffcore_rename::Warnings>,
}

struct DisplayOpts<'a> {
    /// `--show-signature` / `--no-show-signature` (`rev_info.show_signature`).
    show_signature: bool,
    /// `log.abbrevCommit` / `--abbrev-commit`: abbreviate the `commit <id>` line.
    abbrev_commit: bool,
    /// `log.date` / `--date=<mode>`: the format of the `Date:` line.
    date_mode: DateMode,
    /// `log.showRoot` / `--root`: whether a root commit's diff against the empty
    /// tree is shown (default true).
    show_root: bool,
    /// The `--diff-merges` mode a merge commit is rendered under, already resolved
    /// against `--first-parent` by `show_setup_revisions_tweak()`.
    merges: super::log::DiffMerges,
    /// `--remerge-diff` / `--diff-merges=remerge` (`revs->remerge_diff`), which
    /// this port refuses when it reaches a merge — see [`show_commit`].
    remerge: bool,
    /// Set the moment a merge is reached under `remerge`, so the run can print
    /// `git log`'s fatal and leave at 128 instead of writing a partial record.
    remerge_hit: &'a std::cell::Cell<bool>,
    /// Byte ranges of the output that `--line-prefix` must not reach.
    ///
    /// `show_raw_diff()` prints `diff_line_prefix(opt)` only on its `--raw` branch
    /// (combine-diff.c:1244), so a merge's combined `--name-only`/`--name-status`
    /// records go out unprefixed while everything around them is prefixed.
    no_prefix: &'a std::cell::RefCell<Vec<(usize, usize)>>,
    /// `--exit-code` (`o->flags.exit_with_status`): whether the run reports its
    /// findings in the exit status. It also makes `log_tree_diff()`'s
    /// `all_need_diff` true on its own, so the change queue is built even under
    /// `-s`.
    exit_code: bool,
    /// `(o->flags.has_changes, o->flags.check_failed)` accumulated over every
    /// record — the two bits `diff_result_code()` reads.
    status: &'a std::cell::Cell<(bool, bool)>,
    /// The diff options the patch body is rendered with (`-U<n>`, `-w`, prefixes).
    patch: super::diff::PatchOpts,
    /// `--stat` width geometry (see [`StatWidths`]).
    stat: StatWidths,
    /// `--compact-summary`: annotate each stat row with ` (new|gone|mode ±x|…)`.
    compact_summary: bool,
    /// `--relative`'s prefix, already narrowed against; empty when the flag is off.
    /// `strip_prefix()` (diff.c:5009) removes it from the patch, raw, name and stat
    /// writers only.
    relative: String,
    /// `struct dirstat_opts` — the `--dirstat[=<params>]` / `--dirstat-by-file` /
    /// `--cumulative` parameter block, read only when the format is selected.
    dirstat: super::diff_files::DirStat,
    /// `--decorate` / `log.decorate`: the decoration style for the `commit <id>`
    /// and oneline headers. `Off` appends nothing.
    decorate: DecorateStyle,
    /// The commit→refs map behind `decorate`; `None` when decorations are off.
    decorations: Option<&'a Decorations>,
    /// `--use-mailmap` / `log.mailmap`: rewrites the `Author:`/`Commit:` lines
    /// through `.mailmap`. `None` shows the identity as the commit recorded it.
    mailmap: Option<&'a Mailmap>,
    /// The mailmap `%aN`/`%aE`/`%cN`/`%cE` resolve through. `format_person_part()`
    /// consults it whether or not `--use-mailmap` is on, so a format that names
    /// one loads it even under `--no-use-mailmap`.
    identity_mailmap: Option<&'a Mailmap>,
    /// `get_commit_format`'s terminator/separator answer for the selected format,
    /// which is `show_log()`'s `opt->use_terminator`.
    terminator: bool,
    /// `git log`'s header formatter, which `cmd_show` shares: it owns the
    /// abbreviation cache so a multi-commit `git show A..B` shortens each id once.
    renderer: &'a super::log::EntryRenderer<'a>,
    /// `opt->diffopt.needed_rename_limit` / `degraded_cc_to_c` as they stood when
    /// each rendered commit finished.
    ///
    /// `cmd_show` reports them through `diff_result_code()`, but where that runs
    /// depends on the shape of the request (builtin/log.c:696-701, 745-754): a walk
    /// is one `cmd_log_walk()` for the whole range, so only the final state is
    /// reported, while a no-walk request runs `cmd_log_walk_no_free()` once per
    /// pending commit and reports after each. `rev.diffopt` is *not* reset between
    /// those calls (`rev.diffopt.no_free = 1`), so a commit whose own pass never
    /// reached the candidate check still reports the previous commit's numbers.
    rename_warn: &'a std::cell::RefCell<RenameWarnState>,
    /// The notes trees whose `Notes[ (<ref>)]:` block follows the message.
    notes: &'a [super::notes::Tree],
    /// Whether the notes display is on at all — see `RenderCtx::notes_shown`.
    notes_shown: bool,
    /// `-z`: NUL instead of newline as the record terminator, and paths written
    /// raw rather than through `write_name_quoted()`. It reaches the header's own
    /// terminator too — `git show --name-status -z --format=%H` ends the id with a
    /// NUL — and, for a merge's combined record, the separator that follows it.
    z: bool,
    /// `revs->expand_tabs_in_log`, when `--expand-tabs[=<n>]`/`--no-expand-tabs`
    /// set one. `None` leaves the indented header formats on
    /// `expand_tabs_in_log_default`, which is 8.
    expand_tabs: Option<usize>,
    /// `show_log()`'s `ctx.rev = opt; ctx.print_email_subject = 1;`
    /// (log-tree.c:700-701) — the subject prefix and RFC2047 switch the mail
    /// formats read. `cmd_show` runs the same `cmd_log_init`, so unlike
    /// `rev-list` it has both.
    email: super::log::EmailStyle<'a>,
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

/// Parse an integer with git's lenient `strtoul`-ish behavior for a `--stat*=<n>`
/// value; a non-numeric value leaves the slot at its "unset" sentinel.
fn parse_stat_i64(s: &str) -> i64 {
    s.parse::<i64>().unwrap_or(-1)
}


/// Pickaxe search (`-S`/`-G`): limits the shown diff to file pairs whose change
/// text matches, as git's `diffcore-pickaxe` does. Filtering is per file, so a
/// commit that touched several files shows only the ones that match.
struct Pickaxe {
    /// `-S<string>`: a filepair hits when the needle's count differs between the
    /// two sides (a net add or remove). A literal unless `--pickaxe-regex`.
    s: Option<super::diff_pickaxe::Needle>,
    /// `-G<regex>`: a filepair hits when any added/removed line matches.
    g: Option<regex::bytes::Regex>,
    /// `--pickaxe-all` (`DIFF_PICKAXE_ALL`): when any pair in the queue matched,
    /// `diffcore_pickaxe()` keeps the *whole* queue instead of only the matches —
    /// so the commit shows every file it touched. When nothing matched the queue is
    /// emptied either way.
    all: bool,
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
                // `cmd_show`'s `case OBJ_COMMIT:` runs one `cmd_log_walk_no_free()`
                // per pending commit, and that closes with `diff_result_code()`
                // (builtin/log.c:745-754) — so each printed commit gets its own
                // rename-limit report, off the shared `rev.diffopt`.
                let cur = disp.rename_warn.borrow().current;
                disp.rename_warn.borrow_mut().per_commit.push(cur);
                break;
            }
            Kind::Tag => {
                let target = show_tag(out, &obj, pretty, disp, shown_one)?;
                obj = repo.find_object(target)?;
            }
        }
    }
    Ok(())
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
///
/// `show_tag_object()` sends the `tagger` line through `pp_user_info(&pp,
/// "Tagger", …)`, so the identity block is the selected pretty format's, not one
/// shape for every format (pretty.c:516-595):
///
/// ```c
/// if (pp->fmt == CMIT_FMT_ONELINE)
///         return;
/// …
/// if (cmit_fmt_is_mail(pp->fmt)) {
///         … "From: " … "\nDate: %s\n" …
/// } else {
///         strbuf_addf(sb, "%s: %.*s%.*s\n", what, …);
/// }
/// …
/// switch (pp->fmt) {
/// case CMIT_FMT_MEDIUM:
///         strbuf_addf(sb, "Date:   %s\n", show_ident_date(&ident, &pp->date_mode));
///         break;
/// …
/// ```
///
/// So `oneline` prints no identity at all; the mail formats print `From:` with an
/// RFC2822 `Date:` that `--date=` does not reach; `fuller` pads the label to the
/// `AuthorDate:` column and adds `TaggerDate:` (the `%sDate: ` arm, whose `what`
/// is `Tagger`); `medium` adds `Date:   `; and `short`, `full`, `raw`,
/// `reference` and a user format print `Tagger:` with no date line at all.
fn show_tag(
    out: &mut Vec<u8>,
    obj: &gix::Object<'_>,
    pretty: &Pretty,
    disp: &DisplayOpts<'_>,
    shown_one: &mut bool,
) -> Result<ObjectId> {
    let tag = obj.try_to_tag_ref()?;
    // `--date=relative` measures against the wall clock, as `show_date_relative()`
    // does.
    let now = super::log::now_secs();

    // `case OBJ_TAG:`'s `if (rev.shown_one) putchar('\n');` (builtin/log.c:715-716),
    // which separates a tag from whatever record came before it — the same
    // unconditional newline `case OBJ_TREE:` writes, not the pretty format's
    // terminator.
    if *shown_one {
        out.push(b'\n');
    }
    out.extend_from_slice(b"tag ");
    out.extend_from_slice(tag.name);
    out.push(b'\n');

    match (tag.tagger()?, pretty) {
        (_, Pretty::Oneline) | (None, _) => {}
        (Some(tagger), Pretty::Email | Pretty::MboxRd) => {
            let mut sb = String::new();
            super::log::write_identity_headers_for(
                &mut sb,
                &tagger,
                disp.email.encode_headers,
            )?;
            out.extend_from_slice(sb.as_bytes());
        }
        (Some(tagger), _) => {
            out.extend_from_slice(b"Tagger: ");
            // `pp_user_info()` pads the label out to the `TaggerDate: ` column
            // under `fuller`, the same four spaces it gives `Author:`.
            if matches!(pretty, Pretty::Fuller) {
                out.extend_from_slice(b"    ");
            }
            out.extend_from_slice(tagger.name);
            out.extend_from_slice(b" <");
            out.extend_from_slice(tagger.email);
            out.extend_from_slice(b">\n");
            // Only `CMIT_FMT_MEDIUM` and `CMIT_FMT_FULLER` reach a `Date:` arm of
            // the switch; every other non-mail format falls off its end with
            // nothing written.
            match pretty {
                Pretty::Medium => {
                    let t = tagger.time()?;
                    let date = super::log::fmt_time(t.seconds, t.offset, disp.date_mode.clone(), now);
                    writeln!(out, "Date:   {date}")?;
                }
                Pretty::Fuller => {
                    let t = tagger.time()?;
                    let date = super::log::fmt_time(t.seconds, t.offset, disp.date_mode.clone(), now);
                    writeln!(out, "TaggerDate: {date}")?;
                }
                _ => {}
            }
        }
    }

    // `show_tag_object()` stops its header scan at the blank line and then
    // `fwrite`s the rest of the object verbatim — the blank line included — so the
    // record ends exactly where the object does. `cmd_show` then sets
    // `rev.shown_one = 1` (builtin/log.c:722), which is what puts the blank line
    // between the tag and the object it points at under a separator format; a
    // terminator format (`oneline`, `tformat:`) gets none, and adding one here
    // unconditionally is what used to leave `git show --pretty=oneline <tag>` with
    // a stray blank line.
    out.push(b'\n');
    out.extend_from_slice(tag.message);
    if !tag.message.ends_with(b"\n") {
        out.push(b'\n');
    }
    *shown_one = true;

    Ok(tag.target())
}

/// `log_tree_diff()`'s merge dispatch (log-tree.c:1131-1173), which decides how
/// many records one commit produces and what each of them diffs against.
///
/// `separate` is the only mode that repeats the record: its loop runs once per
/// parent, re-entering `show_log()` each time with `log->parent` set, so a
/// two-parent merge prints two full records. Every other mode prints one.
/// `-L` short-circuits ahead of all of this (log-tree.c:1108-1112), so a
/// line-level request keeps its single record whatever the mode says.
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
    let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
    if parents.len() > 1 && line_log_pairs.is_none() {
        // `do_remerge_diff()` (log-tree.c:1134-1142) is where git re-runs the merge
        // and diffs its result against the recorded tree. This port has no merge
        // engine to re-run, so the request is refused at exactly the point the
        // bytes would be wrong — a run that reaches no merge is unaffected, which
        // is what git's own placement gives.
        if disp.remerge {
            disp.remerge_hit.set(true);
            return Ok(());
        }
        if disp.merges == super::log::DiffMerges::Separate {
            for p in &parents {
                show_commit_record(
                    repo, out, commit, pretty, selection, pathspecs, disp, pickaxe, source,
                    shown_one, line_log_pairs, Some(*p),
                )?;
            }
            return Ok(());
        }
    }
    show_commit_record(
        repo, out, commit, pretty, selection, pathspecs, disp, pickaxe, source, shown_one,
        line_log_pairs, None,
    )
}

/// One record of a commit: the header in the selected pretty format, the
/// separator, then the selected diff output.
///
/// `from` is `log->parent` (log-tree.c:1149): `Some` for each per-parent record
/// of `--diff-merges=separate`/`-m`, which diffs against that parent and carries
/// the ` (from <oid>)` header insert. `None` is every other record, whose diff is
/// against the first parent (or, for a merge under `combined`/`dense-combined`,
/// against all of them at once).
#[allow(clippy::too_many_arguments)]
fn show_commit_record(
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
    from: Option<ObjectId>,
) -> Result<()> {
    let parents: Vec<_> = commit.parent_ids().collect();
    let is_merge = parents.len() > 1;
    // The tree this record diffs against: the named parent for a `separate`
    // record, otherwise the first parent (`None` for a root commit, whose diff is
    // against the empty tree).
    let against: Option<ObjectId> = from.or_else(|| parents.first().map(|p| p.detach()));
    // `if (opt->combine_merges) return do_diff_combined(opt, commit);`
    // (log-tree.c:1144-1145) — the whole merge in one section set, headed
    // `diff --cc` or `diff --combined` as `dense_combined_merges` picks.
    let combined = is_merge
        && matches!(
            disp.merges,
            super::log::DiffMerges::Combined | super::log::DiffMerges::DenseCombined
        );
    // The `} else return 0;` after it: a merge under neither `combine_merges` nor
    // `separate_merges` produces no diff at all, and `log_tree_commit()`'s
    // `always_show_header` then prints the header alone (log-tree.c:1151-1152,
    // 1191-1195). `--stat`, `--raw` and the rest are suppressed with it.
    let merge_off = is_merge && disp.merges == super::log::DiffMerges::Off;
    // An empty user format (`--format=`) prints no header at all, and git then
    // omits the blank line that would separate the header from the diff.
    let header_empty = matches!(pretty, Pretty::User(f) if f.is_empty());

    // Resolve the file-level changes up front — before the header — so `-S`/`-G`
    // can suppress the ENTIRE commit (header included) when the pickaxe matches no
    // file, exactly as git does. `files` is computed only when a diff would be
    // shown (a real diff selection, and either a non-root commit or `--root`).
    // `revision.c:3149-3152` raises `revs->diff` for the pickaxe, `--diff-filter` and
    // `--follow` alike ("Pickaxe, diff-filter and rename following need diffs"), so
    // the queue is built under `-s` too — which is what lets `-S<needle>` suppress a
    // commit that matched nothing even when nothing would have been printed.
    let diff_shown = (selection != Selection::Disabled || disp.exit_code || pickaxe.active())
        && !(parents.is_empty() && !disp.show_root)
        && !merge_off;
    // `diffcore_pickaxe()` runs inside `diffcore_std()`, which
    // `find_paths_generic()` (combine-diff.c:1378-1420) calls once per parent — so
    // it reaches a combined merge too, and this two-way queue is the one the `i ==
    // 0` pass filters (the queue the count formats are measured on).
    let pickaxe_path = pickaxe.active();
    let mut queue_nonempty = false;
    let files: Vec<FileChange> = if line_log_pairs.is_some() {
        // `-L` renders `line_log_queue_pairs()`' pairs, not the commit's own change
        // set, so none of the collection below runs.
        Vec::new()
    } else if diff_shown {
        let mut warn = super::diffcore_rename::Warnings::default();
        let mut f = collect_changes(repo, commit, against, &disp.patch, &mut warn)?;
        // Only a pass that reached `too_many_rename_candidates()` writes the
        // `diff_options` fields (see `git log`'s `record_rename_warnings`); an
        // empty report leaves the previous commit's in place.
        if warn.needed_rename_limit != 0 || warn.degraded_cc_to_c {
            disp.rename_warn.borrow_mut().current = warn;
        }
        if !pathspecs.is_empty() {
            let specs = super::log::PathspecMatcher::new(repo, pathspecs)?;
            f.retain(|c| specs.matches(&c.path));
        }
        if pickaxe_path {
            // Test each file's own change text, exactly as `git log` tests a
            // commit's patch. `--pickaxe-all` then decides what survives: git keeps
            // the whole queue when anything matched and empties it when nothing did
            // (`diffcore_pickaxe()`), where the default keeps just the matches.
            let hit: Vec<bool> = f
                .iter()
                .map(|c| {
                    let mut buf = Vec::new();
                    emit_patch(repo, &mut buf, c).is_ok()
                        && super::log::pickaxe_hit_needle(
                            &buf,
                            pickaxe.s.as_ref(),
                            pickaxe.g.as_ref(),
                        )
                })
                .collect();
            match pickaxe.all && hit.iter().any(|&h| h) {
                true => {}
                false => {
                    let mut it = hit.into_iter();
                    f.retain(|_| it.next().unwrap_or(false));
                }
            }
        }
        // `log_tree_diff_flush()` tests the queue for emptiness here, before
        // `diff_flush()` re-renders it quietly under a whitespace rule and drops the
        // pairs whose patch came out empty. A whitespace-only commit therefore still
        // separates its message from the diff it no longer prints.
        // `--relative[=<path>]`'s narrowing half (`diff_queue()`'s prefix test,
        // diff.c:7630), which every format sees; the shortening half is `strip_prefix`
        // and is applied per writer below, because `diff_summary()` and
        // `show_dirstat()` never call it.
        if !disp.relative.is_empty() {
            f.retain(|c| c.path.starts_with(disp.relative.as_bytes()));
        }
        // `diffcore_apply_filter()`: the name and stat formats report the same
        // filtered queue the patch renders.
        if let Some(filter) = &disp.patch.diff_filter {
            f.retain(|c| super::diff::diff_filter_selected(filter, c.status));
        }
        queue_nonempty = !f.is_empty();
        if disp.patch.ws != super::diff::Whitespace::Keep {
            f.retain(reports_change);
        }
        f
    } else {
        Vec::new()
    };
    // `o->flags.has_changes`, which `diff_result_code()` turns into the `01` bit of
    // the exit status. Two shapes never set it: `diff_tree_combined()` has no
    // `has_changes` assignment at all, so `git show --exit-code` on a merge under
    // `-c`/`--cc` reports 0 however much the merge changed; and `--check`, whose
    // `diff_flush_checkdiff()` reports through `check_failed` instead — which is
    // why `--check --exit-code` is 2 rather than 3. Under a whitespace rule the
    // queue is re-tested after the quiet re-render (`diff_from_contents`), so a
    // change that came out empty does not count.
    if !combined && selection != Selection::Check {
        let changed = match disp.patch.ws != super::diff::Whitespace::Keep {
            true => !files.is_empty(),
            false => queue_nonempty,
        };
        if changed {
            disp.status.set((true, disp.status.get().1));
        }
    }
    // `diff_tree_combined()` calls `show_log()` before it has scanned a single path
    // (combine-diff.c:1506-1516), so a merge whose queue the pickaxe emptied still
    // prints its header; only the two-way path can be suppressed outright.
    if pickaxe_path && diff_shown && files.is_empty() && line_log_pairs.is_none() && !combined {
        return Ok(());
    }
    // `cmd_log_init_finish()` (builtin/log.c:333) clears `always_show_header` when
    // `--diff-filter` is in play, so a commit whose queue the filter emptied prints
    // nothing at all — not even its header.
    if disp.patch.diff_filter.is_some() && diff_shown && files.is_empty() {
        return Ok(());
    }

    // `show_log()`: a separator format puts the record terminator in front of
    // every record but the first — what separates two commits of
    // `git show A..B` — while a terminator format already closed the previous
    // record with one (log-tree.c:776-793). Under `-z` that byte is a NUL, since
    // it is `opt->diffopt.line_termination` in both places.
    let rec_term = if disp.z { b'\0' } else { b'\n' };
    if *shown_one && !disp.terminator {
        out.push(rec_term);
    }
    *shown_one = true;

    // `cmd_show` runs the same `cmd_log_init` and prints through the same
    // `show_log()` as `cmd_log`, so the header is rendered by `git log`'s own
    // formatter — every format name and `%` placeholder behaves identically
    // across the two commands because it is literally the same code.
    disp.renderer.render(
        out,
        commit,
        pretty,
        &super::log::ShowEntry {
            abbrev_commit: disp.abbrev_commit,
            date_mode: disp.date_mode.clone(),
            decorate: disp.decorate,
            decorations: disp.decorations,
            mailmap: disp.mailmap,
            identity_mailmap: disp.identity_mailmap,
            notes: disp.notes,
            notes_shown: disp.notes_shown,
            expand_tabs: disp.expand_tabs,
            email: disp.email,
            source: source.map(str::as_bytes),
            show_signature: disp.show_signature,
            from,
        },
    )?;
    // The closing half of `show_log()`: a terminator format ends each record with
    // `opt->diffopt.line_termination`, except the genuinely empty user format,
    // which emits nothing at all (log-tree.c:915-919).
    if disp.terminator && !header_empty {
        out.push(rec_term);
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
            // `-L` refuses `--check` outright above, so no record reaches here
            // under it.
            Selection::Disabled | Selection::Check => {}
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
                    emit_raw(repo, out, &files, disp.z, &disp.relative)?;
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

    // `--diff-merges=off` on a merge: `log_tree_diff()` returned before it queued
    // anything, so this record is the header and nothing else.
    if merge_off {
        return Ok(());
    }

    if combined {
        // `find_paths_generic()` (combine-diff.c:1378-1420) runs `diffcore_std()` —
        // and so `diffcore_pickaxe()` — against each parent in turn and intersects
        // what survives, so a path reaches a combined section only when it hit the
        // pickaxe against *every* parent. The combined set is already the
        // intersection of the unfiltered per-parent sets, so testing each path
        // against each parent and keeping the ones that hit everywhere is the same
        // answer. `None` means no pickaxe: nothing to narrow.
        let survivors: Option<Vec<Vec<u8>>> = match pickaxe.active() {
            false => None,
            true => Some(combined_pickaxe_survivors(
                repo, commit, &parents, pathspecs, disp, pickaxe,
            )?),
        };
        // The pathspec limit the combined writers take. Under a pickaxe it is the
        // surviving path list; an empty one means "no path at all", which an empty
        // pathspec vector would instead read as "no limit", so the writers below are
        // skipped outright in that case.
        let combined_paths: Vec<String> = match &survivors {
            Some(paths) => paths.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect(),
            None => pathspecs.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect(),
        };
        let combined_empty = survivors.as_ref().is_some_and(Vec::is_empty);
        // git shows a blank line after a merge's message regardless of format,
        // then the combined diff against all parents, plus `--stat` (against the
        // first parent) when requested. The empty user format prints neither the
        // blank line nor a header. `first-parent` and `separate` opt out: the
        // merge falls through to the plain two-way path below, diffing against
        // one parent like any commit.
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
            let parent_ids: Vec<ObjectId> = parents.iter().map(|p| p.detach()).collect();
            let sep = if disp.z { b'\0' } else { b'\t' };
            let end = if disp.z { b'\0' } else { b'\n' };
            let start = out.len();
            let rows = match combined_empty {
                true => Vec::new(),
                false => super::diff::merge_combined_names(
                    repo,
                    commit.id().detach(),
                    &parent_ids,
                    &combined_paths,
                )?,
            };
            for (path, letters) in rows {
                if status {
                    out.extend_from_slice(letters.as_bytes());
                    out.push(sep);
                }
                out.extend_from_slice(&name_bytes(&path, disp.z));
                out.push(end);
            }
            // `show_raw_diff()` prints the line prefix on its `--raw` branch only
            // (combine-diff.c:1244), so these records stay unprefixed.
            if out.len() != start {
                disp.no_prefix.borrow_mut().push((start, out.len()));
            }
            return Ok(());
        }
        if let Selection::Blocks {
            raw,
            numstat,
            stat,
            shortstat,
            dirstat,
            summary,
            patch,
        } = selection
        {
            let ps: &[String] = &combined_paths;
            // `diff_tree_combined()`'s `needsep` (combine-diff.c:1600-1610). It is
            // set by which format *bits* are on, not by whether they wrote
            // anything — which is why `git show --summary -p` on a merge whose
            // summary is empty still puts a blank line in front of the patch.
            let mut needsep = false;
            // `STAT_FORMAT_MASK` (combine-diff.c:1371-1375): the count formats are
            // computed solely against the first parent, inside
            // `find_paths_generic()`'s `i == 0` pass — so they run over the very
            // two-way queue `files` already holds, and they run *before* the raw
            // block below. Their order among themselves is `diff_flush()`'s.
            if numstat {
                emit_numstat(out, &files, &disp.relative, disp.z);
                needsep = true;
            }
            if stat {
                emit_stat(out, &files, &disp.stat, disp.compact_summary, &disp.relative, &disp.patch.colors)?;
                needsep = true;
            }
            if shortstat {
                emit_shortstat(out, &files)?;
                needsep = true;
            }
            if dirstat {
                super::diff::commit_dirstat(
                    repo,
                    commit.id,
                    against,
                    &disp.patch,
                    None,
                    &disp.dirstat,
                    out,
                )?;
                needsep = true;
            }
            if summary {
                emit_summary(out, &files)?;
                needsep = true;
            }
            // `show_raw_diff()` over the combined path set, which replaces the
            // two-way `--raw` block entirely and, per the `else if` at
            // combine-diff.c:1604, is the branch that owns `needsep` when both it
            // and a count format are asked for.
            if raw {
                if !combined_empty {
                    out.extend_from_slice(&super::diff::merge_combined_raw(
                        repo,
                        commit.id,
                        &parents.iter().map(|p| p.detach()).collect::<Vec<_>>(),
                        ps,
                        combined_raw_abbrev(repo),
                        disp.z,
                        true,
                    )?);
                }
                needsep = true;
            }
            let wrote = needsep;
            if patch {
                // Dense combined diff of the merge's tree against every
                // parent tree, rendered by the shared `diff --cc` engine.
                let result_tree = commit.tree()?;
                let mut parent_trees = Vec::with_capacity(parents.len());
                for p in &parents {
                    parent_trees.push(repo.find_commit(p.detach())?.tree()?);
                }
                // `show_combined_header()` (combine-diff.c:944) heads each section
                // `diff --cc` under `dense_combined_merges` and `diff --combined`
                // without it, and `make_hunks()` (combine-diff.c:621) skips its
                // uninteresting-hunk pass in the same non-dense case.
                let cc = match combined_empty {
                    true => Vec::new(),
                    false => super::diff::combined_trees_patch_painted(
                        repo,
                        &result_tree,
                        &parent_trees,
                        ps,
                        3,
                        disp.merges == super::log::DiffMerges::DenseCombined,
                        &disp.patch.colors,
                    )?,
                };
                // `printf("%s%c", diff_line_prefix(opt), opt->line_termination)`
                // (combine-diff.c:1610): the separator is the record terminator, so
                // `-z` makes it a NUL. It is written on `needsep` alone, whether or
                // not the combined patch turns out to have sections.
                if wrote {
                    out.push(if disp.z { b'\0' } else { b'\n' });
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
    // blank line. A mail format that already fenced its notes block with `---`
    // has raised `opt->shown_dashes`, which suppresses this second one.
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
            ) if !super::log::mail_notes_shown_dashes(
                repo,
                disp.notes,
                pretty,
                commit.id().detach(),
            )? =>
            {
                out.extend_from_slice(b"---\n")
            }
            _ => out.push(b'\n'),
        }
    }

    match selection {
        Selection::Disabled => {}
        // `diff_flush_checkdiff()` in place of every other format. A combined merge
        // never reaches here — `diff_tree_combined()` returned above without ever
        // looking at `DIFF_FORMAT_CHECKDIFF`.
        Selection::Check => {
            let specs: Vec<String> = pathspecs
                .iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
            if super::diff::commit_check(repo, out, commit.id, against, &disp.patch, &specs)? {
                disp.status.set((disp.status.get().0, true));
            }
        }
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
                        out.extend_from_slice(&name_bytes(shorten_path(source, &disp.relative), disp.z));
                    }
                    out.push(sep);
                }
                out.extend_from_slice(&name_bytes(shorten_path(&f.path, &disp.relative), disp.z));
                out.push(end);
            }
        }
        Selection::Blocks {
            raw,
            numstat,
            stat,
            shortstat,
            dirstat,
            summary,
            patch,
        } => {
            // `diff_flush()`'s `separator` counter, not "the buffer is non-empty":
            // `show_dirstat()` (diff.c:7238) writes without raising it, so
            // `--dirstat -p` runs the patch straight on with no blank line, while
            // `--dirstat=lines` — emitted from inside the count-format block at
            // diff.c:7233 — does earn one.
            let mut wrote_block = false;
            // `diff_flush()`'s fixed order: raw, numstat, stat, shortstat, summary,
            // then the patch.
            if raw {
                emit_raw(repo, out, &files, disp.z, &disp.relative)?;
                wrote_block = true;
            }
            if numstat {
                emit_numstat(out, &files, &disp.relative, disp.z);
                wrote_block = true;
            }
            // `diff_flush()` tests the two bits separately, so `--stat --shortstat`
            // prints the stat block and then a second summary line.
            if stat {
                emit_stat(out, &files, &disp.stat, disp.compact_summary, &disp.relative, &disp.patch.colors)?;
                wrote_block = true;
            }
            if shortstat {
                emit_shortstat(out, &files)?;
                wrote_block = true;
            }
            // `diff_flush()`: dirstat sits between the stat formats and the summary.
            if dirstat {
                if disp.dirstat.by_line {
                    wrote_block = true;
                }
                super::diff::commit_dirstat(
                    repo,
                    commit.id,
                    against,
                    &disp.patch,
                    None,
                    &disp.dirstat,
                    out,
                )?;
            }
            if summary {
                let before = out.len();
                emit_summary(out, &files)?;
                // `!is_summary_empty(q)` guards the `separator++`.
                wrote_block |= out.len() != before;
            }
            if patch {
                // `diff_flush()`'s DIFF_SYMBOL_SEPARATOR between an already-written
                // block and the patch is `o->line_termination` (diff.c:1436-1440), so
                // `-z` makes it a NUL rather than a blank line.
                if wrote_block {
                    out.push(rec_term);
                }
                // The patch body comes from the shared `git diff` pipeline (the same
                // one `git log -p` renders through), so every diff option it takes —
                // `-w`, `-U<n>`, `--full-index`, the prefixes — applies here too, and
                // the two commands stay byte-identical.
                //
                // `diffcore_pickaxe()` filters the *queue*, and every format renders
                // the filtered queue — the patch included. The shared pipeline takes
                // no pickaxe, so the surviving paths are handed to it as the limit
                // instead; both sides of a rename go in, since limiting the tree
                // diff to the destination alone would hide the deletion the pair
                // needs.
                let specs: Vec<String> = match pickaxe_path {
                    true => files
                        .iter()
                        .flat_map(|f| {
                            std::iter::once(f.path.clone())
                                .chain(f.source.iter().cloned())
                        })
                        .map(|p| String::from_utf8_lossy(&p).into_owned())
                        .collect(),
                    false => pathspecs
                        .iter()
                        .map(|p| String::from_utf8_lossy(p).into_owned())
                        .collect(),
                };
                out.extend_from_slice(
                    &super::diff::commit_patches(
                        repo,
                        &[(commit.id, against)],
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
    // `opt->diffopt.needed_rename_limit` / `degraded_cc_to_c`: one struct for the
    // whole command, so each commit's rename pass overwrites the last — and
    // `diff_result_code()` reports whatever the final one left behind.
    warn: &mut super::diffcore_rename::Warnings,
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
    *warn = detect_renames(repo, &mut out, opts)?;
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
) -> Result<super::diffcore_rename::Warnings> {
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
        return Ok(super::diffcore_rename::Warnings::default());
    }
    let opts = super::diffcore_rename::Options {
        detect_rename: detect,
        rename_score: popts.rename_score,
        find_copies_harder: popts.find_copies_harder,
        break_opt: popts.break_opt,
        rename_empty: popts.rename_empty,
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

    let mut content = super::diffcore_rename::OdbContent { repo };
    // `too_many_rename_candidates()` records what limit would have sufficed in
    // `opt->diffopt`, and `diff_result_code()` prints it once when the command
    // ends — `cmd_show` returns through it just as `cmd_log` does.
    let warnings = super::diffcore_rename::run(&mut q, &opts, &mut content);
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
    Ok(warnings)
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
fn emit_raw(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    files: &[FileChange],
    z: bool,
    rel: &str,
) -> Result<()> {
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
            out.extend_from_slice(&name_bytes(shorten_path(source, rel), z));
        }
        out.push(sep);
        out.extend_from_slice(&name_bytes(shorten_path(&f.path, rel), z));
        out.push(end);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// --stat
// ---------------------------------------------------------------------------

/// The rows [`super::diffstat::show_stats`] renders. For a binary file the two
/// "counts" are the byte sizes, which is what `builtin_diffstat()` stores.
fn stat_rows(files: &[FileChange], compact: bool, rel: &str) -> Vec<diffstat::StatFile> {
    files
        .iter()
        .map(|f| diffstat::StatFile {
            print_name: stat_name(f, compact, rel),
            added: if f.is_binary { f.new_content.len() as u64 } else { f.added as u64 },
            deleted: if f.is_binary { f.old_content.len() as u64 } else { f.deleted as u64 },
            binary: f.is_binary,
            // `show` renders committed pairs, so nothing here is ever unmerged.
            is_unmerged: false,
        })
        .collect()
}

/// `--stat` (`show_stats()`), rendered by the shared port.
fn emit_stat(
    out: &mut Vec<u8>,
    files: &[FileChange],
    sw: &StatWidths,
    compact: bool,
    rel: &str,
    colors: &diff_color::DiffColors,
) -> Result<()> {
    diffstat::show_stats(out, &stat_rows(files, compact, rel), sw, colors);
    Ok(())
}

/// `diff_opt_char()` (diff.c:5593): store `arg[0]` in the named
/// `output_indicators[]` slot, refusing anything longer than one byte. An empty
/// value stores the NUL that terminates it, which `emit_line_0()` declines to write.
fn set_indicator(
    opts: &mut super::diff::PatchOpts,
    name: &str,
    val: &str,
) -> std::result::Result<(), String> {
    if val.len() > 1 {
        return Err(format!("error: {} expects a character, got '{val}'", &name[2..]));
    }
    let c = val.as_bytes().first().copied().unwrap_or(0);
    match name {
        "--output-indicator-new" => opts.indicators.0 = c,
        "--output-indicator-old" => opts.indicators.1 = c,
        _ => opts.indicators.2 = c,
    }
    Ok(())
}



/// `--numstat` (`show_numstat()`, diff.c:3243-3277): added, deleted, name — with
/// `-` counts for a binary pair, and the rename form for a moved file.
///
/// The name half has two shapes, and `options->line_termination` picks between
/// them (diff.c:3261-3276). With a newline terminator the single `print_name`
/// `fill_print_name()` built is written — `<from> => <to>` for a rename, quoted
/// where `quote_c_style()` would quote. Under `-z` the terminator is NUL and the
/// pair is split instead: a NUL, the pre-image path, a NUL, the post-image path,
/// a NUL — three fields for a rename and one for everything else, none of them
/// quoted, since `write_name_quoted()` writes the name raw when its terminator
/// is NUL.
fn emit_numstat(out: &mut Vec<u8>, files: &[FileChange], rel: &str, z: bool) {
    for f in files {
        if f.is_binary {
            out.extend_from_slice(b"-\t-\t");
        } else {
            out.extend_from_slice(format!("{}\t{}\t", f.added, f.deleted).as_bytes());
        }
        if z {
            if let Some(source) = &f.source {
                out.push(b'\0');
                out.extend_from_slice(shorten_path(source, rel));
                out.push(b'\0');
            }
            out.extend_from_slice(shorten_path(&f.path, rel));
            out.push(b'\0');
            continue;
        }
        out.extend_from_slice(&stat_name(f, false, rel));
        out.push(b'\n');
    }
}

/// `--shortstat` (`show_shortstats()`): the `--stat` summary line on its own.
fn emit_shortstat(out: &mut Vec<u8>, files: &[FileChange]) -> Result<()> {
    diffstat::show_shortstats(out, &stat_rows(files, false, ""));
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

/// The name the stat formats print for a file: a rename shows both sides through
/// `pprint_rename()`, which factors out a shared prefix and suffix
/// (`dir/{old => new}.txt`) and otherwise prints `old => new`.
/// `strip_prefix()` (diff.c:5009): advance a reported name past `--relative`'s
/// prefix. Called only from the writers git calls it from.
fn shorten_path<'a>(path: &'a [u8], rel: &str) -> &'a [u8] {
    match path.starts_with(rel.as_bytes()) {
        true => &path[rel.len()..],
        false => path,
    }
}

fn stat_name(f: &FileChange, compact: bool, rel: &str) -> Vec<u8> {
    let mut name = match &f.source {
        // `pprint_rename()` quotes the pair itself; a plain name goes through
        // `quote_c_style()` in `fill_print_name()`.
        Some(source) => {
            super::diff_pairs::pprint_rename(shorten_path(source, rel), shorten_path(&f.path, rel))
        }
        None => super::diff_files::quoted_name_bytes(shorten_path(&f.path, rel)),
    };
    // `--compact-summary`'s ` (<comment>)` suffix (`fill_print_name()`).
    if compact {
        if let Some(c) = super::diff::compact_comment_for_modes(f.old_mode, f.new_mode) {
            name.push(b' ');
            name.push(b'(');
            name.extend_from_slice(c.as_bytes());
            name.push(b')');
        }
    }
    name
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

/// The paths a combined merge's sections survive the pickaxe with.
///
/// `find_paths_generic()` (combine-diff.c:1378-1420) diffs the merge against each
/// parent in turn, runs `diffcore_std()` — pathspec, rename detection and
/// `diffcore_pickaxe()` — over that queue, and intersects the surviving path sets.
/// So a path is kept exactly when it hit the pickaxe against every parent, and
/// `--pickaxe-all` widens that per parent: a parent whose queue matched anywhere
/// contributes all of its paths.
fn combined_pickaxe_survivors(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    parents: &[gix::Id<'_>],
    pathspecs: &[Vec<u8>],
    disp: &DisplayOpts<'_>,
    pickaxe: &Pickaxe,
) -> Result<Vec<Vec<u8>>> {
    let specs = match pathspecs.is_empty() {
        true => None,
        false => Some(super::log::PathspecMatcher::new(repo, pathspecs)?),
    };
    let mut survivors: Option<Vec<Vec<u8>>> = None;
    for parent in parents {
        let mut warn = super::diffcore_rename::Warnings::default();
        let mut f = collect_changes(repo, commit, Some(parent.detach()), &disp.patch, &mut warn)?;
        if let Some(specs) = &specs {
            f.retain(|c| specs.matches(&c.path));
        }
        let hit: Vec<bool> = f
            .iter()
            .map(|c| {
                let mut buf = Vec::new();
                emit_patch(repo, &mut buf, c).is_ok()
                    && super::log::pickaxe_hit_needle(&buf, pickaxe.s.as_ref(), pickaxe.g.as_ref())
            })
            .collect();
        let keep_all = pickaxe.all && hit.iter().any(|&h| h);
        let kept: Vec<Vec<u8>> = f
            .iter()
            .zip(hit)
            .filter(|(_, h)| keep_all || *h)
            .map(|(c, _)| c.path.clone())
            .collect();
        survivors = Some(match survivors {
            None => kept,
            Some(prev) => prev.into_iter().filter(|p| kept.contains(p)).collect(),
        });
    }
    Ok(survivors.unwrap_or_default())
}

/// The width the combined `--raw` columns abbreviate object names to. Same
/// `core.abbrev`-derived width [`short_oid`] uses for the two-way form, since
/// `show_raw_diff()` (combine-diff.c:1228) prints through
/// `find_unique_abbrev()` with `opt->abbrev` just as `diff_flush_raw()` does.
fn combined_raw_abbrev(repo: &gix::Repository) -> usize {
    crate::abbrev::configured_abbrev(repo, repo.object_hash().len_in_hex()).max(MINIMUM_ABBREV)
}

/// [`super::diff::apply_line_prefix`] over everything outside `exempt`.
///
/// The exempt ranges are whole records, each ending in its own terminator, so
/// splitting the buffer at their edges and prefixing each surviving piece on its
/// own reproduces `emit_line_0()`'s placement exactly: a piece gets a prefix at its
/// first byte and after each of its interior newlines, and the byte that follows an
/// exempt record is the first byte of the next piece.
fn apply_line_prefix_except(out: Vec<u8>, prefix: &[u8], exempt: &[(usize, usize)]) -> Vec<u8> {
    if prefix.is_empty() || exempt.is_empty() {
        return super::diff::apply_line_prefix(out, prefix);
    }
    let mut spans: Vec<(usize, usize)> = exempt.to_vec();
    spans.sort_unstable();
    let mut res: Vec<u8> = Vec::with_capacity(out.len());
    let mut at = 0usize;
    for (start, end) in spans {
        if start > at {
            res.extend_from_slice(&super::diff::apply_line_prefix(out[at..start].to_vec(), prefix));
        }
        res.extend_from_slice(&out[start..end]);
        at = end;
    }
    if at < out.len() {
        res.extend_from_slice(&super::diff::apply_line_prefix(out[at..].to_vec(), prefix));
    }
    res
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
// Dates
// ---------------------------------------------------------------------------
//
// `cmd_show` runs `cmd_log_init`, so `--date=`/`log.date` are parsed and rendered
// by `git log`'s own `parse_date_mode`/`fmt_time` — a second copy here could only
// drift from them.

