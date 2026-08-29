//! `git format-patch` — render commits as mbox-style e-mail patches.
//!
//! Each message is built the way stock git builds it: the fixed
//! `From <oid> Mon Sep 17 00:00:00 2001` magic line, the `From:`/`Date:`/
//! `Subject:` headers (RFC2822 date, RFC2047 q-encoded headers when needed,
//! RFC822 wrapping), the commit body, the three-dash separator, the
//! `--stat`+`--summary` block at git's 72-column mail width, the patch itself,
//! and the `-- \n<version>\n\n` signature.
//!
//! The diffstat is a faithful port of git's `show_stats()` (diff.c) including
//! `scale_linear()` graph scaling and the name-column ellipsis, and the summary
//! lines are a port of `diff_summary()`. The patch body reuses the same default
//! diff settings the rest of this crate uses: Myers with the indent (slider)
//! heuristic, three lines of context, `@@`-header function context, and the
//! `\ No newline at end of file` marker. The slider pass is a default rather than a
//! constant — `--no-indent-heuristic` clears it, reaching
//! [`super::diff_pairs::compute_compacted`]'s `indent_heuristic` argument.
//!
//! Covered:
//!   * revision selection — `<since>` (implicit `<since>..HEAD`), `<a>..<b>`,
//!     `<a>...<b>`, `^<rev>`, `-<n>`, `--root`; merges are excluded as git does.
//!     The `<since>` shorthand fires on `rev.pending.nr == 1` — the count of
//!     pending *objects*, which includes an excluded one, so `format-patch
//!     ^<rev>` walks `<rev>..HEAD` just as `format-patch <rev>` does while
//!     `^<a> <b>` is a two-endpoint range and is left alone. With no revision
//!     argument at all `s_r_opt.def` supplies HEAD, which is what makes a bare
//!     `format-patch` silent (`HEAD..HEAD`) and an unborn branch there fatal.
//!   * pseudo-revisions — the `--all` family `handle_revision_pseudo_opt()`
//!     seeds into the same pending list: `--all`, `--branches`, `--tags`,
//!     `--remotes`, each with an optional `=<glob>`, plus `--glob=<pattern>`,
//!     `--exclude=<glob>` (which feeds the next selector and is then cleared)
//!     and `--not`. Each is seeded where it stands on the command line, because
//!     that is the order the walk's commit-date queue breaks ties in, and each
//!     counts as revision input even when it matches nothing.
//!   * the `SYMMETRIC_LEFT` selectors — `--cherry-pick`, `--cherry-mark`,
//!     `--left-only`, `--right-only` and `--cherry`. `cherry_pick_list()`
//!     (revision.c:1217) computes patch ids for the smaller side of a symmetric
//!     range and drops every equal-patch-id commit from *both* sides;
//!     `limit_left_right()` (revision.c:1421) then keeps the side that was named.
//!     Both run inside `limit_list()`, i.e. before the ordering flags and before
//!     `--skip`/`-<n>` cut the list down, and both are inert on a range with only
//!     one side. `--cherry-pick --right-only` is the selector `git rebase --apply`
//!     drives (builtin/rebase.c:668).
//!   * `--pretty=email` / `--format=email` (what this module already renders) and
//!     `--pretty=mboxrd`, which is that plus `pp_remainder()`'s `>` escape on a
//!     `/^>*From /` body line (pretty.c:2286). As a *pretty format* it is not gated
//!     on `--stdout`, unlike the `format.mboxrd` config (builtin/log.c:2253). Every
//!     other `--pretty` value is a different renderer and stays refused.
//!   * revision errors — git's own `fatal: ambiguous argument …` / `bad object`
//!     / `bad revision` / `Invalid revision range` text on stderr with exit 128,
//!     and a positional that names an existing path is a pathspec, not an error.
//!   * output — file-per-patch (default, names printed to stdout) or `--stdout`.
//!   * walk ordering — the `setup_revisions()` family format-patch inherits:
//!     `--topo-order`, `--date-order`, `--author-date-order` (git's three
//!     `sort_in_topological_order()` tie-breaks), `--no-walk[=(sorted|unsorted)]`
//!     / `--do-walk`, and `--reverse`. `cmd_format_patch` emits its `list[]`
//!     backwards, so each of them comes out as the reverse of what `git log`
//!     would print and `--reverse` cancels that flip rather than adding to it.
//!   * flags — `--stdout`, `-o`/`--output-directory` (a callback, so a second one
//!     is `fatal: two output directories?` rather than "last wins" — see
//!     [`set_outdir`]), `-<n>`/`--max-count`,
//!     `--skip`, `--reverse`, `--min-parents`/`--max-parents`/`--no-merges`,
//!     `-n`/`--numbered`, `-N`/`--no-numbered`, `--start-number`,
//!     `--numbered-files`, `--suffix`, `--subject-prefix`, `--rfc`,
//!     `-v`/`--reroll-count`, `--signature`/`--no-signature`,
//!     `--signature-file`, `--zero-commit`, `-p`/`--no-stat`, `--root`,
//!     `-q`/`--quiet`, `--filename-max-length`, `--cover-letter`,
//!     `-k`/`--keep-subject`, `--to`, `--cc`, `--add-header`, `--in-reply-to`,
//!     `-U`/`--unified`, `-a`/`--text`, `--minimal`, `--patience`,
//!     `--histogram`, `--diff-algorithm=<name>` (every name
//!     `parse_algorithm_value()` takes, `default` and mixed case included).
//!   * messaging — `-s`/`--signoff` (a port of `append_signoff()` with its
//!     trailer-block dedup), `--from[=<ident>]` and `--force-in-body-from` (the
//!     in-body `From:` `pp_user_info()` emits when the header identity is
//!     replaced), `--thread[=shallow|deep]` (`Message-ID:` plus the
//!     `In-Reply-To:`/`References:` chain), `--attach[=<boundary>]` /
//!     `--inline[=<boundary>]` (the `multipart/mixed` wrapper and the
//!     `text/x-patch` part `diffopt.stat_sep` hangs the patch off),
//!     `--notes[=<ref>]`/`--no-notes` (the `Notes:` commentary block, which is
//!     what collapses the `---` before the diffstat to a bare blank line), and
//!     `--base=<commit>|auto`/`--no-base` (the `base-commit:` trailer and the
//!     `prerequisite-patch-id:` list, via a port of `diff_get_patch_id()`).
//!   * cover letter — `--cover-from-description=<mode>`, `--description-file`
//!     and the `branch.<name>.description` lookup behind them, plus
//!     `--commit-list-format=shortlog|modern|log:<fmt>|<fmt>`. Its magic `From`
//!     line names `make_cover_letter()`'s `head = list[0]`, and its combined
//!     diffstat runs only when the walk left exactly one boundary commit —
//!     git's "we can only do diffstat with a unique reference point".
//!   * alternate diffstat formats — `--stat`, `--summary`, `--numstat`,
//!     `--shortstat`, and the whole dirstat family (`--dirstat[=<params>]`,
//!     `-X<params>`, `--dirstat-by-file`, `--cumulative`), selected the way
//!     git's `diff_flush()` selects them and separated the way `log_tree_diff()`
//!     separates them (`---` only when the diffstat and the patch are both on).
//!   * merges the parent-count bounds admit. `--max-parents=<n>` (n > 1) or
//!     `--min-parents` can put a merge in the series, and `log_tree_diff()`
//!     produces nothing for one without `-m`/`-c`/`--cc`, none of which
//!     format-patch sets by default: `log_tree_commit()`'s `always_show_header`
//!     fallback then emits the mail headers and the message alone. The
//!     `--diff-merges=<mode>` family (and `-m`, `-c`, `--dd`) changes that.
//!     `separate`/`on`/`m` emits one whole message per parent — `log_tree_diff()`
//!     re-enters `show_log()` for each, so the header block repeats, with
//!     `show_log()`'s inter-record newline between them. `first-parent`/`1`/`--dd`
//!     stops after the first parent. `combined`/`c` and `dense-combined`/`cc` go
//!     through `diff_tree_combined()`: the three-dash separator becomes the bare
//!     newline `show_combined_diff()` writes, every *stat* format is computed
//!     against the first parent alone (`STAT_FORMAT_MASK`, combine-diff.c:1368),
//!     `--raw` becomes `show_raw_diff()`'s `::`-prefixed block, and the patch body
//!     is [`super::diff::merge_combined_patch_painted`]'s `diff --combined` /
//!     `diff --cc` sections. `log.diffMerges` redefines what `on`/`m`/`-m` mean and
//!     nothing else — measured against stock 2.55.0, it does not give a merge a
//!     diff on its own.
//!   * `--color[=<when>]` / `--no-color`, and with it the whole paint layer:
//!     `--word-diff[=<mode>]`, `--word-diff-regex=<re>`, `--color-words[=<re>]`,
//!     `--color-moved[=<mode>]`, `--color-moved-ws=<modes>` and
//!     `--ws-error-highlight=<kinds>`, all through [`super::diff_color`]'s
//!     `colorize_patch_ex` — the same re-emit pass `git diff` and `git log -p` use,
//!     fed the whole commit's patch at once so a block moved *between* two of its
//!     files is still recognized. Colour reaches the diffstat graph
//!     (`show_stats()`) and the patch body, which is everything format-patch
//!     paints; `--numstat`, `--raw` and `diff_summary()`'s lines stay plain.
//!     `--color-words` and `--word-diff=color` force colour on outright, so they
//!     paint to a pipe. The *switch* answers to the flag and the terminal alone:
//!     measured against stock 2.55.0, neither `color.diff`, `diff.color` nor
//!     `color.ui` moves it in either direction, though the `color.diff.<slot>`
//!     palette, `core.whitespace`, `diff.wsErrorHighlight`, `diff.colorMoved`,
//!     `diff.colorMovedWS` and `diff.wordRegex` are all read.
//!   * `--inter-hunk-context=<n>` (xdiff's `interhunkctxlen`, which widens
//!     `xdl_get_hunk()`'s `max_common` to `ctxlen + ctxlen + interhunkctxlen`) and
//!     `--ignore-blank-lines` (`XDF_IGNORE_BLANK_LINES`, whose
//!     `xdl_mark_ignorable_lines()` verdict `xdl_mark_ignorable_regex()` never
//!     overrides — 2.55.0's regex pass opens `if (xch->ignore) continue;`, so the
//!     two markers are an or).
//!   * `--diff-filter=<letters>`, through [`super::diff_filter`]: a faithful
//!     `diff_opt_diff_filter()` + `diffcore_apply_filter()` including the `B`
//!     (broken, i.e. a `-B`-scored modification) and `*` (all-or-none) letters and
//!     `diff_setup_done()`'s exclusion-only fold.
//!   * `--relative[=<path>]` / `--no-relative` over `diff.relative`. Both halves:
//!     `diff_queue()`'s prefix test narrows the queue *before* `diffcore_rename()`
//!     runs — so a rename out of the prefix is reported as a plain creation — and
//!     `strip_prefix()` shortens the names the patch, the diffstat and the raw
//!     block print, but not the ones `diff_summary()` or `show_dirstat()` print.
//!   * `--submodule[=<format>]`: `short` diffs the synthetic `Subproject commit
//!     <oid>` blobs this renderer already emits, while `log` and `diff` divert a
//!     gitlink pair's whole section — `diff --git` header included — to
//!     `show_submodule_diff_summary()` / `show_submodule_inline_diff()`. The
//!     diffstat is built by a pass with no such branch, so its row stays the
//!     `short` one. `diff.submodule` is *not* consulted, measured against stock.
//!   * `--line-prefix=<p>`, applied per emitter rather than to the message as a
//!     whole. Every line of a patch takes it, because `show_log()` opens with
//!     `graph_show_commit()` and `graph_show_strbuf()` writes it between message
//!     lines even with a NULL graph (graph.c:74-80, 1533); the `base-commit:`
//!     trailer and the signature `cmd_format_patch` prints itself do not, nor does
//!     `DIFF_SYMBOL_STAT_SEP`'s `--attach` MIME part, nor an interdiff/range-diff
//!     block (`show_diff_of_diff()` installs its own `output_prefix`). A cover
//!     letter never reaches `show_log()`, so only `log_write_email_headers()`'
//!     three `graph_show_oneline()` calls and its diffstat carry it.
//!   * width-tuned diffstat — `--stat=<width>[,<name-width>[,<count>]]`,
//!     `--stat-width`, `--stat-name-width`, `--stat-graph-width`,
//!     `--stat-count`, a port of `diff_opt_stat()`'s field parsing and the
//!     column scaling / `--stat-count` ` ...` truncation in `show_stats()`.
//!   * `-I<regex>`/`--ignore-matching-lines=<regex>`, via a vendored POSIX ERE
//!     engine (`regcomp(REG_EXTENDED | REG_NEWLINE)` semantics) and a port of
//!     xdiff's `xdl_get_hunk()` hunk selection.
//!   * `-W`/`--function-context`, xdiff's `XDL_EMIT_FUNCCONTEXT`: each hunk grows
//!     back to the line the enclosing function starts on and forward to the line
//!     before the next one, merging into the following hunk when the two meet.
//!   * `diffcore_std()`'s rename/copy/break passes, through
//!     [`super::diffcore_rename`]: `-M`/`--find-renames[=<n>]` and
//!     `--no-renames`, `-C`/`--find-copies[=<n>]`, `--find-copies-harder` (which
//!     also feeds the trees' shared blobs into the queue as copy sources, the way
//!     `diff_tree_paths()` stops skipping them), `-B`/`--break-rewrites[=<n>[/<m>]]`
//!     with the `dissimilarity index`, ` rewrite <path> (<n>%)` and `M<nnn>` lines
//!     a surviving score prints, `--[no-]rename-empty`, and `-l<n>`.
//!   * the raw output formats — `--raw`, `--patch-with-raw` and
//!     `--patch-with-stat` (`diff_flush_raw()`, ahead of every stat block), and
//!     `--compact-summary`'s ` (new)`/` (gone)`/` (mode +x)` stat annotations.
//!   * `--abbrev[=<n>]`, clamped by `handle_revision_opt()` and then grown per
//!     object until unique. `--full-index` overrides it in the `index` line but
//!     not in the raw one, which is the split `diff_flush_raw()` has.
//!   * `--output-indicator-new/-old/-context=<char>` and `--expand-tabs[=<n>]`
//!     (`strbuf_add_tabexpand()` over the log message, measured in display
//!     columns; format-patch's own default is 0).
//!   * the four output formats format-patch cannot have. `--name-only`,
//!     `--name-status`, `--check` and `--remerge-diff` / `--diff-merges=remerge`
//!     each `die()` after `setup_revisions()` has resolved the revisions
//!     (builtin/log.c:2220-2227), and two of them together are rejected one step
//!     earlier by `diff_setup_done()` (diff.c:5259-5261).
//!   * `--interdiff=<rev>` and `--range-diff=<rev>` with `--creation-factor=<n>`:
//!     `show_diff_of_diff()`'s two trailing blocks, either behind the single
//!     patch (indented two spaces, as `log-tree.c` indents the interdiff) or in
//!     the cover letter (flush left), including the `Interdiff against v<n>:` /
//!     `Range-diff against v<n>:` titles a `-v<n>` reroll selects, the ranges
//!     `infer_range_diff_ranges()` derives, and the rule that either option turns
//!     a cover letter on for a multi-patch series.
//!
//! Flags git accepts that are *not* ported are recorded during parsing and
//! rejected only once it is clear a patch would actually be emitted. Rejecting
//! early would report a porting gap for an invocation git itself refuses, so the
//! two implementations would disagree about *why* they failed. Nothing is
//! silently ignored: if the commit list is non-empty the unported flag is still
//! fatal.
//!
//! Error precedence mirrors git's two passes. Format-patch's own options
//! (`--start-number`, `--thread`, `--cover-from-description`, …) are validated in
//! `parse_options` and so preempt everything, whatever their position. The diff
//! options and the revisions then share `setup_revisions`, a single
//! left-to-right pass, so a bad diff-option *value* (`--color=`, `--diff-algorithm=`,
//! `--stat=`, `--ignore-submodules=`, the `--max-parents=`/`--max-count=`/… integer
//! counts) and a bad revision race by command-line position: whichever comes
//! first wins. These value errors are therefore not emitted in place — they are
//! recorded in `Opts::opt_error` with their argument index and resolved against
//! the revisions in `select_commits`, so `format-patch --color=bad HEAD~9` is the
//! colour error (129) while `format-patch HEAD~9 --color=bad` is the revision
//! error (128). git's own exit taxonomy is preserved: 129 for an option value
//! parse-options rejects, 128 for a `die()` (bad revision, bad `--ignore-submodules`
//! word, a count that is `'not an integer'`).
//!
//! A binary pair renders as the base85 `GIT binary patch` payload
//! ([`super::binary_patch`]) that format-patch's implied `--binary` calls for, with both
//! blobs named in full in the `index` line (`fill_metainfo()` raises the abbreviation to
//! `hexsz` there); `--no-binary` leaves `Binary files … differ` instead. Either way the
//! stat row is `Bin <n> -> <m> bytes` and `--numstat` prints `-` for both counts.
//!
//! Not covered — these `bail!` rather than emit output that would diverge:
//!   * pathspec-limited output. A pathspec is parsed and honoured to the extent
//!     that it never becomes a bogus revision error, but limiting the walk and
//!     the patch to it is not ported, so a pathspec that reaches a non-empty
//!     commit list is fatal.
//!   * `--anchored=<text>`, blocked on the vendored differ: the anchors would have
//!     to reach xdiff's patience pass, and `gix-imara-diff`'s `patience.rs` omits
//!     every anchor branch (its header records that `is_anchor()` is constantly
//!     false), so accepting the flag would silently drop them.
//!   * `--textconv`/`--ext-diff`, which need a `gitattributes` diff driver to run.
//!     `--ignore-if-in-upstream` reproduces
//!     everything `cmd_format_patch` decides before the comparison — the
//!     single-endpoint promotion that turns a lone rev into `<rev>..HEAD`, the
//!     silent exit when both endpoints are the same commit, and the two
//!     refusals (`need exactly one range`, `not a range`) — but the patch-id
//!     comparison itself is not ported, so a real range is still refused.
//!   * `--src-prefix=<p>`, `--dst-prefix=<p>`, `--no-prefix`, `--default-prefix` and
//!     `--output=<file>` *are* ported; unknown options report git's own
//!     `fatal: unrecognized argument: <arg>` (128).
//!   * a glob in `notes.displayRef`/`GIT_NOTES_DISPLAY_REF`/`--notes=<glob>`:
//!     expanding a pattern over the ref store is not ported, so a pattern is
//!     refused rather than read as a literal ref name.
//!   * the rest of `handle_revision_pseudo_opt()`: `--reflog`, `--bisect`,
//!     `--indexed-objects`, `--alternate-refs`, `--exclude-hidden=<section>`,
//!     `--filter=<spec>`, `--single-worktree` and `--stdin`. Each still reports
//!     `unrecognized argument`, which is a real divergence rather than a
//!     deferral. Relatedly, `--all` here is every ref plus `HEAD`, without the
//!     `other_head_refs()` pass that adds a linked worktree's HEAD — so in a
//!     repository with linked worktrees it is already what `--single-worktree`
//!     would have asked for.
//!
//! ### Config
//!
//! The `format.*` keys read here are the defaults for the options above:
//! `outputDirectory`, `numbered`, `suffix`, `subjectPrefix`, `signature`,
//! `signatureFile`, `filenameMaxLength`, `coverLetter`, `to`, `cc`, `headers`,
//! `encodeEmailHeaders` (the `--[no-]encode-email-headers` default; on),
//! `noprefix` (the `--no-prefix` default), `signOff`, `from`,
//! `forceInBodyFrom`, `thread`, `attach`, `notes`, `coverFromDescription`,
//! `commitListFormat` and `useAutoBase`. `format.mboxrd` has no command-line
//! spelling at all in format-patch and is read directly. `format.pretty` belongs
//! to `log`/`show`, not here. `branch.<name>.description`, `core.notesRef` and
//! `notes.displayRef` are read through the options that consult them.
//!
//! A generated `Message-ID` embeds `time(NULL)` and the committer's address, so
//! `--thread` output is reproducible only up to that timestamp; everything the
//! id then feeds — the `In-Reply-To:` target, the `References:` chain and how
//! shallow and deep threading differ — is byte-identical to git's.
//!
//! Known deviations, stated rather than hidden: a *type change* (a regular file
//! that became a symlink, or the reverse) renders as one pair with `old mode`/
//! `new mode` rather than the delete-plus-create `diff_flush_patch()` splits it
//! into. `--word-diff-regex`/`--color-words` compile their pattern while the
//! command line parses, whereas `init_diff_words_data()` compiles it at the first
//! blob diff — so an unparsable pattern exits 128 with git's own message but
//! without the mail headers and diffstat git has already flushed by then. The
//! `whitespace_rule()` a colour pass applies comes from `core.whitespace` alone,
//! with no per-path `whitespace` attribute, which is the same reach `git diff`'s
//! colour path has here. The cover letter's shortlog does not wrap long subjects at 76 columns.
//! `append_signoff()`'s trailer-block scan does not consult `trailer.<token>.key`
//! config, so only git's own generated prefixes can carry a mixed block over the
//! 25% threshold. The ERE
//! engine matches over Unicode scalar values decoded from the line (invalid
//! UTF-8 bytes decode to themselves), where a C library in a `C` locale would
//! match byte-wise; the two agree for every ASCII pattern. It is also permissive
//! about the constructs POSIX leaves undefined and the C libraries disagree on —
//! an empty alternation branch (`(a|)b`), a stacked repetition (`a**`) and a
//! dangling range (`[a-c-e]`) compile here and under glibc, while BSD `regcomp`
//! rejects all three; every pattern both accept produces the same answer.

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, ByteSlice};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};
use gix::hash::ObjectId;
use gix::object::tree::diff::ChangeDetached;
use gix::prelude::ObjectIdExt;
use gix::revision::walk::Sorting;

use super::diff_color::{self, ColorWhen, DiffColors, ExtraPaint, FilePaint, MoveWordOpts, PaintOptions};
use super::diffstat::{self, StatWidths};
use gix::traverse::commit::simple::CommitTimeOrder;

impl Opts {
    /// The prefix `--relative` is narrowing and shortening by, or `None` when the
    /// flag is off. An empty prefix — a bare `--relative` at the top of the worktree
    /// — matches everything and strips nothing, which is what git's zero
    /// `prefix_length` does.
    fn active_relative(&self) -> Option<&str> {
        self.relative_name.then_some(self.relative_prefix.as_str())
    }
}

/// `usage_with_options()` over `builtin/log.c`'s `format-patch` option table.
const USAGE: &str = r"usage: git format-patch [<options>] [<since> | <revision-range>]

    -n, --[no-]numbered   use [PATCH n/m] even with a single patch
    -N, --no-numbered     use [PATCH] even with multiple patches
    -s, --[no-]signoff    add a Signed-off-by trailer
    --[no-]stdout         print patches to standard out
    --[no-]cover-letter   generate a cover letter
    --[no-]commit-list-format <format-spec>
                          format spec used for the commit list in the cover letter
    --[no-]numbered-files use simple number sequence for output file names
    --[no-]suffix <sfx>   use <sfx> instead of '.patch'
    --[no-]start-number <n>
                          start numbering patches at <n> instead of 1
    -v, --[no-]reroll-count <reroll-count>
                          mark the series as Nth re-roll
    --[no-]filename-max-length <n>
                          max length of output filename
    --[no-]rfc[=<rfc>]    add <rfc> (default 'RFC') before 'PATCH'
    --[no-]cover-from-description <cover-from-description-mode>
                          generate parts of a cover letter based on a branch's description
    --[no-]description-file <file>
                          use branch description from file
    --subject-prefix <prefix>
                          use [<prefix>] instead of [PATCH]
    -o, --output-directory <dir>
                          store resulting files in <dir>
    -k, --keep-subject    don't strip/add [PATCH]
    --no-binary           don't output binary diffs
    --binary              opposite of --no-binary
    --[no-]zero-commit    output all-zero hash in From header
    --[no-]ignore-if-in-upstream
                          don't include a patch matching a commit upstream
    -p, --no-stat         show patch format instead of default (patch + stat)

Messaging
    --[no-]add-header <header>
                          add email header
    --[no-]to <email>     add To: header
    --[no-]cc <email>     add Cc: header
    --[no-]from[=<ident>] set From address to <ident> (or committer ident if absent)
    --[no-]in-reply-to <message-id>
                          make first mail a reply to <message-id>
    --[no-]attach[=<boundary>]
                          attach the patch
    --inline[=<boundary>] inline the patch
    --[no-]thread[=<style>]
                          enable message threading, styles: shallow, deep
    --[no-]signature <signature>
                          add a signature
    --[no-]base <base-commit>
                          add prerequisite tree info to the patch series
    --[no-]signature-file <file>
                          add a signature from a file
    -q, --[no-]quiet      don't print the patch filenames
    --[no-]progress       show progress while generating patches
    --[no-]interdiff <rev>
                          show changes against <rev> in cover letter or single patch
    --[no-]range-diff <refspec>
                          show changes against <refspec> in cover letter or single patch
    --[no-]creation-factor <n>
                          percentage by which creation is weighted
    --[no-]force-in-body-from
                          show in-body From: even if identical to the e-mail header

";

/// The version reported in the trailing `-- \n<version>\n` signature. Stock git
/// emits its own `git_version_string` here, so this constant is what makes the
/// signature line comparable; override per-invocation with `--signature=<s>`,
/// `--no-signature`, `--signature-file`, or the
/// `format.signature`/`format.signatureFile` config keys.
const SIGNATURE_VERSION: &str = "2.55.0";

/// git's `MAIL_DEFAULT_WRAP` — the diffstat width used by format-patch.
const MAIL_DEFAULT_WRAP: i64 = 72;

/// git's `FORMAT_PATCH_NAME_MAX_DEFAULT`.
const NAME_MAX_DEFAULT: usize = 64;

/// Header wrap column for `From:`/`Subject:` (RFC2822 §2.1.1).
pub(super) const HEADER_MAX_LENGTH: i64 = 78;

/// The charset name used for RFC2047 encoding and the 8-bit MIME header.
const ENCODING: &str = "UTF-8";

/// git's placeholder subject and body in a generated cover letter.
const COVER_SUBJECT: &str = "*** SUBJECT HERE ***";
const COVER_BLURB: &str = "*** BLURB HERE ***";

/// git's `DIFF_FORMAT_*` bits, restricted to the ones format-patch can emit.
/// `DIFF_FORMAT_PATCH` is not tracked: format-patch always ORs it in, so it
/// would be a constant.
const FMT_DIFFSTAT: u32 = 1 << 0;
const FMT_NUMSTAT: u32 = 1 << 1;
const FMT_SHORTSTAT: u32 = 1 << 2;
const FMT_DIRSTAT: u32 = 1 << 3;
const FMT_SUMMARY: u32 = 1 << 4;
const FMT_RAW: u32 = 1 << 5;

/// git's `diff_dirstat_permille_default` — the 3.0% cut-off.
const DIRSTAT_PERMILLE_DEFAULT: u32 = 30;

/// The dirstat knobs `parse_dirstat_params()` (diff.c) sets.
#[derive(Clone, Copy)]
struct Dirstat {
    /// `lines`: damage is counted in diffstat lines rather than in bytes.
    by_line: bool,
    /// `files`: every changed file contributes exactly one unit of damage.
    by_file: bool,
    /// `cumulative`: a directory that is reported still counts toward its parent.
    cumulative: bool,
    /// The reporting cut-off, in tenths of a percent.
    permille: u32,
}

/// `--signature`/`--no-signature` state, mirroring git's `signature` pointer
/// before resolution: the version default, an explicit value, or suppressed.
enum SigCli {
    Unset,
    No,
    Value(String),
}

/// git's `enum thread_level` — `--thread[=shallow|deep]` / `format.thread`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Thread {
    Unset,
    Shallow,
    Deep,
}

/// git's `enum cover_from_description` — `--cover-from-description=<mode>` /
/// `format.coverFromDescription`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CoverFrom {
    None,
    Message,
    Subject,
    Auto,
}

/// git's `COVER_FROM_AUTO_MAX_SUBJECT_LEN`.
const COVER_FROM_AUTO_MAX_SUBJECT_LEN: usize = 100;

/// git's `enum cover_setting` — the four states `format.coverLetter` can be in.
/// `Unset` and `Off` differ: with `--interdiff`/`--range-diff` over a multi-patch
/// series, `Unset` turns the cover letter on and `Off` leaves it off.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CoverSetting {
    Unset,
    Off,
    On,
    Auto,
}

/// git's `enum auto_base_setting` — `--base=<c>|auto` / `format.useAutoBase`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AutoBase {
    Never,
    Always,
    WhenAble,
}

/// `rev_info`'s `sort_order`/`topo_order` pair, as the ordering flags set it in
/// `revision.c`. Each of the three flags sets `topo_order = 1` and picks the
/// tie-break `sort_in_topological_order()` runs with; without one of them the
/// walk is the plain commit-date priority queue `get_revision()` pops from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    /// No ordering flag given.
    Default,
    /// `--topo-order`: `REV_SORT_IN_GRAPH_ORDER`, a LIFO stack that keeps a
    /// branch contiguous.
    Topo,
    /// `--date-order`: `REV_SORT_BY_COMMIT_DATE`.
    DateTopo,
    /// `--author-date-order`: `REV_SORT_BY_AUTHOR_DATE`.
    AuthorDateTopo,
}

/// One `--all`-family selector, with everything `handle_revision_pseudo_opt()`
/// (revision.c) needs to turn it into pending objects.
///
/// The four namespace forms (`--all`, `--branches`, `--tags`, `--remotes`) go
/// through `handle_refs()`, the `=<pattern>` and `--glob=<pattern>` forms
/// through `refs_for_each_ref_ext()`; both end in `handle_one_ref()`, which
/// drops a ref the pending `--exclude` patterns match and adds the rest.
#[derive(Clone)]
struct RefSet {
    /// The namespace this walks — `refs/heads/` for `--branches`, and so on.
    /// `None` is `--all`/`--glob`, which see the whole ref store.
    prefix: Option<&'static str>,
    /// `=<pattern>`, or `--glob`'s argument. `refs_for_each_ref_ext()` matches
    /// it against the *full* ref name with `prefix` — or, for a `--glob` pattern
    /// that does not already start with it, `refs/` — glued in front, and
    /// appends an implied `/` `*` when the pattern carries no wildcard.
    pattern: Option<String>,
    /// Only `--all` follows its refs with `HEAD`.
    with_head: bool,
    /// The `--exclude=<glob>` patterns standing when this selector was read.
    /// Each selector ends with `clear_ref_exclusions()`, so a list applies to
    /// exactly one of them. They are matched against the name
    /// `handle_one_ref()` receives, which the namespace forms have already
    /// trimmed — hence `--exclude=side --branches` but `--exclude=refs/heads/side
    /// --all`.
    excludes: Vec<String>,
    /// Whether a `--not` was in force, which makes every ref UNINTERESTING.
    negate: bool,
}

/// One revision-ish word of the command line, in the order `setup_revisions()`
/// reads it — which is the order the pending list ends up in, and so the order
/// the walk's commit-date queue breaks ties in.
enum RevWord {
    /// A revision argument: `<rev>`, `^<rev>`, `<a>..<b>` or `<a>...<b>`.
    Rev {
        spec: String,
        /// Where it stood on the command line, which is what orders a revision
        /// error against a diff-option value error.
        pos: usize,
        /// Whether a `--not` was in force, which flips the sense `^` gives it.
        negate: bool,
    },
    /// An `--all`-family selector.
    Refs(RefSet),
}

/// git's `mime_boundary_leader` (log-tree.c).
const MIME_BOUNDARY_LEADER: &str = "------------";

/// An `ident_split` reduced to what the mail formats use: the name and the
/// address, with the timestamp dropped.
#[derive(Clone)]
struct Ident {
    name: String,
    mail: String,
}

/// Port of `split_ident_line()` (ident.c) restricted to the name/mail halves:
/// the name runs to the first `<` with trailing whitespace removed, the address
/// to the first `>` after it. A line with neither is not an identity at all.
fn split_ident_line(line: &str) -> Option<Ident> {
    let b = line.as_bytes();
    let lt = b.iter().position(|&c| c == b'<')?;
    let gt = lt + 1 + b[lt + 1..].iter().position(|&c| c == b'>')?;
    let name_end = b[..lt]
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map_or(0, |i| i + 1);
    Some(Ident {
        name: String::from_utf8_lossy(&b[..name_end]).into_owned(),
        mail: String::from_utf8_lossy(&b[lt + 1..gt]).into_owned(),
    })
}

/// Port of `ident_cmp()` (ident.c): the address decides, the name breaks the
/// tie. Only equality matters to `use_in_body_from()`.
fn ident_eq(a: &Ident, b: &Ident) -> bool {
    a.mail == b.mail && a.name == b.name
}

/// The threading state git keeps on `rev_info`: the id of the message being
/// written and the chain the `References:` header lists.
struct ThreadState {
    message_id: Option<String>,
    refs: Vec<String>,
}

struct Opts {
    // Output shape.
    to_stdout: bool,
    outdir: Option<String>,
    /// Whether `-o`/`--output-directory` was given on the command line, as
    /// opposed to [`Opts::outdir`] having been seeded from
    /// `format.outputDirectory`.
    ///
    /// git keeps the two apart: the option writes the file-scope
    /// `output_directory` (builtin/log.c:1137) and the config writes
    /// `cfg.config_output_directory` (builtin/log.c:895), and the config value is
    /// only folded in at builtin/log.c:2261-2262 — *after* both the
    /// `two output directories?` check in `output_directory_callback()`
    /// (builtin/log.c:1598-1599) and the `--stdout`/`--output`/`--output-directory`
    /// incompatibility check (builtin/log.c:2250-2251). Folding them into one
    /// field here made `format.outputDirectory` behave as if `-o` had been typed,
    /// which refuses `--stdout` in a repository that merely configures an output
    /// directory. This flag is what those two checks read.
    outdir_cli: bool,
    numbered: Option<bool>,
    start_number: usize,
    numbered_files: bool,
    suffix: String,
    subject_prefix: String,
    reroll: Option<String>,
    /// The resolved trailing signature (empty means none). git resolves this
    /// only after `setup_revisions()`, so it is filled in by `resolve_signature`
    /// once the commit list is known, not during parsing.
    signature: String,
    /// `--signature`/`--no-signature` state, git's `signature` variable.
    sig_cli: SigCli,
    /// `--signature-file <path>`, git's `signature_file_arg`; last occurrence wins.
    sig_file_arg: Option<String>,
    /// `format.signature`, git's `cfg.signature`.
    cfg_signature: Option<String>,
    /// `format.signatureFile`, git's `cfg.signature_file`.
    cfg_signature_file: Option<String>,
    zero_commit: bool,
    /// `-p`/`--no-stat`: suppress git's `DIFFSTAT|SUMMARY` default entirely.
    use_patch_format: bool,
    /// The `DIFF_FORMAT_*` bits the caller asked for, before the default fills in.
    output_format: u32,
    dirstat: Dirstat,
    /// `--stat=<w>`/`--stat-width`: the diffstat total width. 0 means git's
    /// format-patch default of `MAIL_DEFAULT_WRAP` (72).
    stat_width: i64,
    /// `--stat-name-width`: cap on the filename column. 0 leaves it uncapped.
    stat_name_width: i64,
    /// `--stat-graph-width`: cap on the `+/-` graph column. 0 leaves it uncapped
    /// (format-patch never sets git's `-1` sentinel, so the config default is
    /// never consulted).
    stat_graph_width: i64,
    /// `--stat-count`: how many files to list before a trailing ` ...` line.
    /// 0 lists every file.
    stat_count: i64,
    quiet: bool,
    name_max: usize,
    cover_letter: bool,
    /// `-k`/`--keep-subject`: keep the commit subject verbatim (newlines and
    /// all), with no `[PATCH]` prefix and no series numbering.
    keep_subject: bool,
    /// `--in-reply-to=<id>`: the cleaned inner message id (without `<`/`>`),
    /// emitted as `In-Reply-To:`/`References:` on every message and the cover.
    in_reply_to: Option<String>,
    /// `--to`/`--cc`: recipient lists, one entry per option occurrence, folded
    /// one entry per continuation line the way git emits them.
    to: Vec<String>,
    cc: Vec<String>,
    /// `--add-header`: extra header lines, emitted verbatim before `To:`/`Cc:`.
    add_header: Vec<String>,
    /// `--[no-]encode-email-headers`, defaulted by `format.encodeEmailHeaders`
    /// (git's default is on): Q-encode `From:`/`Subject:` when they carry
    /// non-ASCII. With it off the raw UTF-8 goes out, and the `MIME-Version:`/
    /// `Content-*` block a non-ASCII message triggers is unaffected.
    encode_email_headers: bool,
    /// `format.mboxrd`: with `--stdout`, escape `/^>*From /` message-body lines
    /// with one more `>` so an mbox reader cannot mistake them for a separator
    /// (`builtin/log.c:2253`, which is where the `--stdout` condition lives).
    mboxrd: bool,
    /// `--pretty=mboxrd`/`--format=mboxrd`: the same escaping as a *pretty format*
    /// rather than as config, so `setup_revisions()` sets `rev.commit_format`
    /// straight to `CMIT_FMT_MBOXRD` and the `--stdout` condition above never
    /// applies. Measured against stock 2.55.0: `--pretty=mboxrd -o <dir>` escapes,
    /// `-c format.mboxrd=true -o <dir>` does not.
    pretty_mboxrd: bool,
    /// `--no-prefix`/`format.noprefix`: drop the `a/`+`b/` path prefixes from
    /// `diff --git`, `---` and `+++`. `--default-prefix` puts them back.
    noprefix: bool,
    /// `--src-prefix=<p>`/`--dst-prefix=<p>`: what those prefixes are when they are
    /// not suppressed. git's defaults are `a/` and `b/`.
    src_prefix: String,
    dst_prefix: String,

    // Messaging.
    /// `-s`/`--signoff`, defaulted by `format.signOff`: append the committer's
    /// `Signed-off-by:` trailer to the message.
    signoff: bool,
    /// `--from[=<ident>]`/`format.from`: the identity the `From:` header names,
    /// with the commit's own author moved into an in-body `From:` when they
    /// differ. `None` leaves the author in the header, as git does by default.
    from: Option<Ident>,
    /// `--force-in-body-from`/`format.forceInBodyFrom`: emit the in-body
    /// `From:` even when it repeats the header identity.
    force_in_body_from: bool,
    /// `--thread[=shallow|deep]`/`format.thread`: generate `Message-ID:` and
    /// chain the series with `In-Reply-To:`/`References:`.
    thread: Thread,
    /// `--attach`/`--inline`/`format.attach`: the MIME multipart boundary.
    mime_boundary: Option<String>,
    /// `--attach`'s `Content-Disposition: attachment` (vs `--inline`'s `inline`).
    no_inline: bool,
    /// `--cover-from-description=<mode>`/`format.coverFromDescription`.
    cover_from: CoverFrom,
    /// `--description-file=<path>`: the branch description to build the cover
    /// letter from, in place of `branch.<name>.description`.
    description_file: Option<String>,
    /// The branch the series was named by, for `branch.<name>.description`.
    branch_name: Option<String>,
    /// `--notes[=<ref>]`/`--no-notes`/`format.notes`.
    notes: super::notes::DisplayOpt,
    /// `--base=<commit>|auto`/`--no-base`/`format.useAutoBase`.
    auto_base: AutoBase,
    /// The explicit `--base=<commit>` argument, if one was given.
    base_commit: Option<String>,
    /// `--commit-list-format=<spec>`/`format.commitListFormat`: how the cover
    /// letter lists the series. `None` is git's `shortlog` default.
    commit_list_format: Option<String>,

    // Revision selection.
    root: bool,
    max_count: Option<usize>,
    skip: usize,
    reverse: bool,
    min_parents: usize,
    max_parents: Option<usize>,
    /// `revs->first_parent_only`: follow only the first parent of each merge.
    /// It changes which commits the walk reaches, not the parent list it reports,
    /// so `max_parents = 1` still drops the merges it walks through.
    first_parent: bool,
    /// `--topo-order`/`--date-order`/`--author-date-order`.
    order: Order,
    /// `revs->no_walk`: list the named commits without traversing to their
    /// parents. It is positional — `--do-walk`, `-<n>`, `--max-count` and any
    /// UNINTERESTING endpoint each clear it again where they sit on the command
    /// line, and a later `--no-walk` turns it back on.
    no_walk: bool,
    /// `revs->unsorted_input`, which only `--no-walk=unsorted` sets: keep the
    /// pending list in command-line order instead of sorting it by commit date.
    unsorted_input: bool,
    /// `revs->cherry_pick` (revision.c:2511): over a symmetric range, drop from
    /// *both* sides every commit whose patch id also appears on the other side.
    /// `cherry_pick_list()` (revision.c:1217) marks them `SHOWN`, which
    /// `get_commit_action()` (revision.c:4178) then suppresses.
    cherry_pick: bool,
    /// `revs->cherry_mark` (revision.c:2509): the same search, but marking
    /// `PATCHSAME` instead of `SHOWN`. `format-patch` has no place to render that
    /// mark, so it changes nothing here — measured against stock 2.55.0,
    /// `--cherry-mark --right-only` and `--right-only` agree byte for byte. It is
    /// still tracked because it is mutually exclusive with `--cherry-pick`.
    cherry_mark: bool,
    /// `revs->left_only` / `revs->right_only` (revision.c:2485-2496), applied by
    /// `limit_left_right()` (revision.c:1421): keep only the side of a symmetric
    /// range the flag names.
    left_only: bool,
    right_only: bool,
    /// Everything that feeds `rev.pending`: the revision arguments and the
    /// `--all`-family selectors, interleaved in command-line order.
    revs: Vec<RevWord>,
    paths: Vec<String>,
    /// `seen_dashdash`, which `setup_revisions()` decides in a pre-pass over argv
    /// before it resolves a single revision. It turns on `REVARG_CANNOT_BE_FILENAME`
    /// for the words *before* the `--` as well, so `format-patch README.md -- x`
    /// is `bad revision 'README.md'` rather than a second pathspec.
    seen_dashdash: bool,
    /// The command line as given, kept so `setup_revisions()`'s `verify_filename()`
    /// sweep can look at the words that follow a positional which turned out to
    /// name a path. git looks at the argv `parse_options()` handed back, which is
    /// this one minus format-patch's own options — see [`fp_option_slots`].
    argv: Vec<String>,
    /// The parsed `-- <pathspec>...` set, built once `setup_revisions()` has
    /// collected `revs->prune_data`. `None` is "no limiting", which is not the
    /// same as a set that matches nothing.
    pathspec: Option<super::log::PathspecMatcher>,

    // Diff rendering.
    context: u32,
    algorithm: Algorithm,
    text: bool,
    /// `--no-binary`: a binary file section stops at `Binary files … differ` instead of
    /// carrying the base85 `GIT binary patch` payload format-patch normally implies.
    no_binary: bool,
    /// `-I<regex>`: change groups whose every line matches one of these are
    /// marked ignorable before hunks are assembled.
    ignore_regex: Vec<Regex>,
    /// `-W`/`--function-context`: `XDL_EMIT_FUNCCONTEXT`, which grows every hunk
    /// outward to the enclosing function's first and last line.
    function_context: bool,
    /// `--indent-heuristic`/`--no-indent-heuristic` (`XDF_INDENT_HEURISTIC`): run
    /// `xdl_change_compact()`.s slider post-processing pass. On by default since
    /// git 2.14, so only `--no-indent-heuristic` clears it.
    indent_heuristic: bool,
    /// `-w`/`-b`/`--ignore-space-at-eol`/`--ignore-cr-at-eol`: xdiff's
    /// `XDF_WHITESPACE_FLAGS`, the canonical form a record is compared in.
    ws: super::diff_pairs::Whitespace,
    /// `--full-index`: `index` lines carry the whole object name rather than the
    /// abbreviation `diff_unique_abbrev()` would pick.
    full_index: bool,
    /// `-D`/`--irreversible-delete`: a deletion stops after its header, so the
    /// patch cannot be used to restore the file.
    irreversible_delete: bool,
    /// `--skip-to=<path>` / `--rotate-to=<path>`, as `(is_skip, path)`. git keeps
    /// one `rotate_to` string plus a `skip_instead_of_rotate` bit, so the last of
    /// the two options on the command line wins.
    skip_or_rotate: Option<(bool, Vec<u8>)>,

    // Rename/copy/break detection — `diffcore_std()`'s first three passes, which
    // this module runs through [`super::diffcore_rename`].
    /// `--find-renames[=<n>]`/`-M`, `--find-copies[=<n>]`/`-C`, `--no-renames`:
    /// git's `diff_options.detect_rename` (diff.c:5722-5756, 6180-6182). `None`
    /// leaves `diff.renames` to decide, which is what a porcelain defaults to.
    detect_rename: Option<u8>,
    /// `-M<n>`/`-C<n>` in `MAX_SCORE` units, as `parse_rename_score()` reads them;
    /// `0` means git's `DEFAULT_RENAME_SCORE` (50%).
    rename_score: u32,
    /// `--find-copies-harder`: unmodified files of the pre-image tree become copy
    /// sources, and `diff_setup_done()` promotes detection to `DIFF_DETECT_COPY`
    /// (diff.c:5288-5289).
    find_copies_harder: bool,
    /// `--rename-empty`/`--no-rename-empty` (diff.c:6183-6184); git's default is on.
    rename_empty: bool,
    /// `-B[<n>][/<m>]` packed as `n | (m << 16)` by `diff_opt_break_rewrites()`
    /// (diff.c:5569-5590); `-1` is break detection off, which is the default.
    break_opt: i64,
    /// `-l<n>`: `diff_options.rename_limit` (diff.c:6188), overriding
    /// `diff.renameLimit`.
    rename_limit: Option<i64>,
    /// `--expand-tabs[=<n>]`: `revs->expand_tabs_in_log`, the tab width
    /// `pp_remainder()` de-tabifies the log message to. format-patch's
    /// `expand_tabs_in_log_default` is 0 (builtin/log.c:2109), so this is off
    /// unless asked for; the bare option means 8 (revision.c:2575-2583).
    expand_tabs: usize,
    /// `--compact-summary`: `diff_options.flags.stat_with_summary`, the
    /// ` (<comment>)` `fill_print_name()` hangs off a `--stat` row.
    compact_summary: bool,
    /// `--inter-hunk-context=<n>`: `diff_options.interhunkcontext`, the extra
    /// distance `xdl_get_hunk()` will bridge before it starts a second hunk
    /// (`max_common = ctxlen + ctxlen + interhunkctxlen`, xdiff/xemit.c:58-60).
    inter_hunk_ctx: i64,
    /// `--ignore-blank-lines` (`XDF_IGNORE_BLANK_LINES`): change groups whose every
    /// pre- and post-image record is blank are marked ignorable, exactly as
    /// `-I<regex>` marks the ones its patterns cover.
    ignore_blank_lines: bool,
    /// `--diff-filter=<letters>` as `(include-bits, exclude-bits)` over
    /// [`super::diff_filter`]'s status set. `None` is no filtering at all.
    diff_filter: Option<super::diff_filter::Filter>,
    /// `--color[=<when>]` / `--no-color`, resolved against `color.diff`/`color.ui`
    /// once the repository is open. Colour reaches the diffstat graph and the patch
    /// body, which is everything `format-patch` paints.
    colors: DiffColors,
    /// `--color-moved[=<mode>]`, `--color-moved-ws=<modes>`, `--word-diff[=<mode>]`,
    /// `--word-diff-regex=<re>` and `--color-words[=<re>]`, layered over
    /// `diff.colorMoved`/`diff.colorMovedWS`/`diff.wordRegex`.
    extra: ExtraPaint,
    /// `--ws-error-highlight=<kinds>`, over `diff.wsErrorHighlight`.
    ws_error_highlight: u32,
    /// `--diff-merges=<mode>` / `-m` / `-c` / `--cc` / `--dd` /
    /// `--no-diff-merges`, over `log.diffMerges` (which only redefines what the
    /// `on`/`m` spelling and `-m` mean). format-patch starts at
    /// [`DiffMerges::Off`].
    diff_merges: DiffMerges,
    /// `--line-prefix=<p>`: `diff_options.line_prefix`, which
    /// `graph_setup_line_prefix()` installs as `output_prefix` (graph.c:347-354).
    /// Every emitter that opens a line with `diff_line_prefix()` — and every
    /// `graph_show_commit()`/`graph_show_oneline()`/`graph_show_strbuf()` call that
    /// writes it for a NULL graph (graph.c:74-80) — takes it; the rest do not.
    line_prefix: Option<String>,
    /// `--relative[=<path>]` / `--no-relative` over `diff.relative`:
    /// `o->flags.relative_name`. `diff_opt_relative()` (diff.c:5905-5914) sets the
    /// flag and only *then* overwrites `o->prefix` when a value came with it, so a
    /// bare `--relative` after a `--relative=<p>` keeps `<p>`.
    relative_name: bool,
    /// `o->prefix`: the command's own prefix (the current directory inside the
    /// repository) until `--relative=<path>` replaces it. Stored verbatim — git
    /// compares `o->prefix_length` bytes and appends no separator, so
    /// `--relative=sr` really does strip two characters off `src/…`.
    relative_prefix: String,
    /// `--submodule[=<format>]` over `diff.submodule`: `short` diffs the synthetic
    /// `Subproject commit <oid>` blobs, `log` and `diff` divert a gitlink pair to
    /// `show_submodule_diff_summary()` / `show_submodule_inline_diff()` instead.
    submodule_format: super::diff::SubmoduleFormat,
    /// `whitespace_rule()` for the run: `core.whitespace` as parsed by
    /// `parse_whitespace_rule()`. Per-path `whitespace` attributes are not consulted,
    /// which is the same reach `git diff`'s own colour path has here.
    ws_rule: u32,
    /// `--output-indicator-new/-old/-context=<char>` as `(new, old, context)`:
    /// `diff_options.output_indicators`, the three bytes `emit_line()` puts in
    /// front of a hunk's body lines (diff.c:6154-6160). Nothing else moves — the
    /// diffstat graph keeps its own `+`/`-`.
    indicators: (u8, u8, u8),
    /// `--abbrev[=<n>]`: `revs->abbrev`, clamped to `[MINIMUM_ABBREV, hexsz]` by
    /// `handle_revision_opt()` (revision.c:2643-2648) and copied onto
    /// `diffopt.abbrev` at revision.c:3172. `None` is git's `DEFAULT_ABBREV`, the
    /// auto length `core.abbrev` picks.
    abbrev: Option<usize>,

    /// `--name-only`, `--name-status`, `--check` and `--remerge-diff` /
    /// `--diff-merges=remerge`: recorded rather than acted on, because
    /// `cmd_format_patch()` only dies on them once `setup_revisions()` has had its
    /// say (builtin/log.c:2220-2227), so a bad revision preempts them.
    name_only: bool,
    name_status: bool,
    check: bool,
    remerge_diff: bool,

    /// Flags git accepts that this module has not ported, in the spelling the
    /// caller used. Reported only when a patch would actually be emitted.
    deferred: Vec<String>,

    /// `--range-diff=<range>`: git validates the range after the walk (128 on a
    /// bad revision).
    range_diff: Option<String>,
    /// `--creation-factor=<n>`, git's `creation_factor`, which starts at -1.
    /// Any value still below zero means the default was never overridden.
    creation_factor: Option<i64>,
    /// `--interdiff=<rev>`, resolved as `parse_opt_object_name()` resolves it.
    /// Only the last entry is used; `--no-interdiff` empties the list.
    interdiff: Vec<ObjectId>,
    /// Whether `--cover-letter`/`--no-cover-letter` was on the command line.
    /// git leaves the flag undecided until the series length is known, because
    /// `--interdiff`/`--range-diff` turn a cover letter on for a multi-patch
    /// series (see the `cover_letter == -1` block in `cmd_format_patch`).
    cover_letter_given: bool,
    /// `format.coverLetter`, kept as git's four-state `enum cover_setting`
    /// rather than a boolean because "unset" and "false" decide differently
    /// once `--interdiff`/`--range-diff` is in play.
    cover_setting: CoverSetting,
    /// `--output=<file>`: the whole series goes to this one file, which is created (and
    /// truncated) while the option parses — before the `--stdout` conflict below.
    output: Option<String>,

    /// The earliest diff-option value error, as `(arg index, exit code, stderr
    /// line)`. git reports these from inside `setup_revisions()`, so a revision
    /// error at an earlier position on the command line preempts it. It is
    /// recorded here during parsing and resolved against the revisions in
    /// `select_commits`, rather than emitted in place.
    opt_error: Option<(usize, u8, String)>,
}

pub fn format_patch(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    let mut opts = match parse(&repo, args)? {
        Parsed::Ready(opts) => *opts,
        Parsed::Exit(code) => return Ok(code),
    };

    // Port of `cmd_format_patch`'s
    //
    //     die_for_incompatible_opt3(use_stdout, "--stdout",
    //                               rev.diffopt.close_file, "--output",
    //                               !!output_directory, "--output-directory");
    //
    // (builtin/log.c:2250-2251). The three ways of naming a destination are
    // mutually exclusive, and `die_for_incompatible_opt4()`
    // (parse-options.c:1528-1558, which the `opt3` form forwards to with a zeroed
    // fourth slot) picks its wording from how many of them were given: three
    // spelled options get the Oxford-comma message, two get the pair message
    // naming them in table order. Checking the pairs separately — which is what
    // this did — printed the pair message for a command line that named all
    // three, and never noticed `--output` together with `--output-directory` at
    // all.
    //
    // The file named by `--output` was already created while the option parsed,
    // as git's `OPT_FILENAME` does, so it is left behind either way.
    //
    // `output_directory` here is the *option's* variable: `format.outputDirectory`
    // is only merged in eleven lines further down (builtin/log.c:2261-2262), so a
    // configured output directory does not make `--stdout` illegal. See
    // [`Opts::outdir_cli`].
    let named: Vec<&str> = [
        (opts.to_stdout, "--stdout"),
        (opts.output.is_some(), "--output"),
        (opts.outdir_cli, "--output-directory"),
    ]
    .into_iter()
    .filter_map(|(given, name)| given.then_some(name))
    .collect();
    match named.as_slice() {
        [a, b, c] => {
            return Ok(fatal(&format!(
                "options '{a}', '{b}', and '{c}' cannot be used together"
            )))
        }
        [a, b] => return Ok(fatal(&format!("options '{a}' and '{b}' cannot be used together"))),
        _ => {}
    }

    // `--ignore-if-in-upstream` compares the series against the other side of a
    // range, so `get_patch_ids()` requires exactly two pending endpoints
    // (`A..B`) and dies `need exactly one range` otherwise.
    //
    // The count it sees is not simply the argument count. `cmd_format_patch`
    // first applies the "traditional `git format-patch origin`" rule: a lone
    // endpoint, with neither `-<n>`/`--max-count` nor `--root` given, is marked
    // UNINTERESTING and `HEAD` is appended — so one endpoint becomes two. That
    // rule is also why a bare `git format-patch --ignore-if-in-upstream` is
    // silent rather than fatal: `s_r_opt.def` supplies `HEAD`, the rule turns it
    // into `HEAD..HEAD`, and the two endpoints resolve to the same object, which
    // git answers with `goto done` — exit 0, no patches.
    if opts.deferred.iter().any(|f| f == "--ignore-if-in-upstream") {
        match upstream_endpoints(&repo, &opts)? {
            Endpoints::Identical => return Ok(ExitCode::SUCCESS),
            Endpoints::Range => {}
            Endpoints::NotOne => return Ok(fatal("need exactly one range")),
            Endpoints::NotARange => return Ok(fatal("not a range")),
        }
    }

    // git: "Make sure 0000-$sub.patch gives non-negative length for $sub".
    let floor = "0000-".len() + opts.suffix.len();
    if opts.name_max <= floor {
        opts.name_max = floor;
    }
    if let Some(r) = &opts.reroll {
        opts.subject_prefix.push_str(&format!(" v{r}"));
    }

    let (commits, paths, pending) = match select_commits(&repo, &opts)? {
        Selected::Commits {
            commits,
            paths,
            pending,
        } => (commits, paths, pending),
        Selected::Exit(code) => return Ok(code),
    };
    // builtin/log.c:2220-2227, immediately after `setup_revisions()`. format-patch
    // always ORs `DIFF_FORMAT_PATCH` into the output format, so an option asking
    // for a *different* format has nowhere to go — and it dies whether or not the
    // walk found anything, which is why this stands ahead of the empty-list exit.
    for (asked, name) in [
        (opts.name_only, "--name-only"),
        (opts.name_status, "--name-status"),
        (opts.check, "--check"),
        (opts.remerge_diff, "--remerge-diff"),
    ] {
        if asked {
            return Ok(fatal(&format!("{name} does not make sense")));
        }
    }
    if commits.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    // `revs->prune_data` is now final, so every diff below — the patches, the
    // diffstats, the summaries, the cover letter's combined diff and the
    // interdiff — is limited to it, exactly as `rev->diffopt.pathspec` limits
    // every format git flushes through it.
    if !paths.is_empty() {
        opts.pathspec = Some(super::log::PathspecMatcher::new(&repo, &paths)?);
    }

    // git resolves the signature only here — after `setup_revisions()` and after
    // confirming the series is non-empty — so a bad revision, and an empty commit
    // list, both preempt an unreadable signature file (an empty range is exit 0,
    // not the file error).
    match resolve_signature(&opts) {
        Ok(sig) => opts.signature = sig,
        Err(code) => return Ok(code),
    }

    let total = commits.len();

    // `cmd_format_patch`'s `cover_letter == -1` block, which only runs once the
    // series length is known: `format.coverLetter=auto` follows the length, an
    // interdiff or range-diff over a multi-patch series turns the cover letter
    // on unless the config switched it off, and otherwise only an explicit
    // `format.coverLetter=true` does.
    if !opts.cover_letter_given {
        let diff_of_diff = !opts.interdiff.is_empty() || opts.range_diff.is_some();
        opts.cover_letter = if opts.cover_setting == CoverSetting::Auto {
            total > 1
        } else if diff_of_diff && total > 1 {
            opts.cover_setting != CoverSetting::Off
        } else {
            opts.cover_setting == CoverSetting::On
        };
    }

    // Both diff-of-diff options need somewhere to put their block: the cover
    // letter, or the one patch of a single-patch series.
    if !opts.interdiff.is_empty() && !opts.cover_letter && total != 1 {
        return Ok(fatal("--interdiff requires --cover-letter or single patch"));
    }
    if opts.range_diff.is_some() && !opts.cover_letter && total != 1 {
        return Ok(fatal("--range-diff requires --cover-letter or single patch"));
    }

    // `creation_factor` starts at -1 and any value still below zero after
    // parsing means "not given", which is also what makes an explicit negative
    // value legal without a `--range-diff` to scale.
    let creation_factor = match opts.creation_factor {
        Some(n) if n >= 0 => {
            if opts.range_diff.is_none() {
                return Ok(fatal("the option '--creation-factor' requires '--range-diff'"));
            }
            n
        }
        _ => super::range_diff::CREATION_FACTOR_FOR_THE_SAME_SERIES,
    };

    // git validates the `--range-diff` range after the walk
    // (`infer_range_diff_ranges`); an unresolvable side dies 128 there, before
    // any supported-but-unported diff option would matter.
    let range_diff_ranges = match opts.range_diff.clone() {
        Some(rd) => {
            if let Err(code) = validate_range_diff(&repo, &rd) {
                return Ok(code);
            }
            Some(infer_range_diff_ranges(&repo, &rd, &commits)?)
        }
        None => None,
    };

    // Everything below emits bytes, so an unported flag can no longer be
    // deferred: it would change what those bytes are.
    if let Some(flag) = opts.deferred.first() {
        bail!("unsupported flag {flag:?}");
    }

    // Auto-numbering kicks in for a series; -n/-N override it. A cover letter
    // always numbers, since it is itself patch 0 of the series.
    let numbered = opts.numbered.unwrap_or(total > 1 || opts.cover_letter);
    let printed_total = if numbered {
        total + opts.start_number - 1
    } else {
        0
    };

    // `get_base_commit()` + `prepare_bases()` run before anything is written, so
    // an unusable base is fatal before the first message.
    let bases = match resolve_bases(&repo, &commits, &opts)? {
        Ok(b) => b,
        Err(code) => return Ok(code),
    };
    let mut bases_pending = bases.is_some();

    // The notes trees `--notes`/`format.notes` selected, loaded once.
    let notes_trees = super::notes::load_display(&repo, &opts.notes)?;

    let mut stdout = std::io::stdout().lock();
    let mut buffered: Vec<u8> = Vec::new();

    let mut th = ThreadState {
        message_id: None,
        refs: opts.in_reply_to.iter().cloned().collect(),
    };

    if opts.cover_letter {
        if opts.thread != Thread::Unset {
            th.message_id = Some(gen_message_id(&repo, "cover")?);
        }
        let mut msg: Vec<u8> = Vec::new();
        // A bad `--commit-list-format` is only caught once the cover letter's
        // headers are already written, so the partial message is emitted first.
        if let Err(code) =
            render_cover_letter(&repo, &commits, &pending, printed_total, &opts, &th, &mut msg)?
        {
            emit_message(&mut buffered, &msg, cover_filename(&opts), &opts)?;
            stdout.write_all(&buffered)?;
            stdout.flush()?;
            return Ok(code);
        }
        // `make_cover_letter()` puts the diff-of-diff blocks straight after the
        // diffstat, flush left (indent 0) and with no blank line of their own —
        // `show_diffstat()` already ended with one.
        if let Err(code) =
            emit_diff_of_diff(&repo, &opts, range_diff_ranges.as_ref(), creation_factor, &commits, 0, &mut msg)?
        {
            emit_message(&mut buffered, &msg, cover_filename(&opts), &opts)?;
            stdout.write_all(&buffered)?;
            stdout.flush()?;
            return Ok(code);
        }
        if let Some(b) = &bases {
            print_bases(&mut msg, b);
            bases_pending = false;
        }
        write_signature(&mut msg, &opts);
        emit_message(&mut buffered, &msg, cover_filename(&opts), &opts)?;
    }

    for (idx, id) in commits.iter().enumerate() {
        let commit = repo.find_object(*id)?.try_into_commit()?;
        let nr = idx + opts.start_number;

        // Port of the threading block in `cmd_format_patch()`: deep threading
        // chains every mail onto the previous one, shallow threading keeps
        // replying to whatever the chain already starts with.
        if opts.thread != Thread::Unset {
            if let Some(mid) = th.message_id.take() {
                let shallow_reuses_head = opts.thread == Thread::Shallow
                    && !th.refs.is_empty()
                    && (!opts.cover_letter || nr > 1);
                if !shallow_reuses_head {
                    th.refs.push(mid);
                }
            }
            th.message_id = Some(gen_message_id(&repo, &id.to_hex().to_string())?);
        }

        let mut msg: Vec<u8> = Vec::new();
        render_message(
            &repo,
            &commit,
            nr,
            printed_total,
            &opts,
            &th,
            &notes_trees,
            &mut msg,
        )?;
        // `log_tree_commit()` calls `show_diff_of_diff()` once the patch body is
        // out, and only for the single-patch case — a cover letter has already
        // carried the blocks, and git clears them before the loop in that case.
        if !opts.cover_letter {
            if let Err(code) =
                emit_diff_of_diff(&repo, &opts, range_diff_ranges.as_ref(), creation_factor, &commits, 2, &mut msg)?
            {
                emit_message(&mut buffered, &msg, patch_filename(&commit, nr, &opts)?, &opts)?;
                stdout.write_all(&buffered)?;
                stdout.flush()?;
                return Ok(code);
            }
        }
        if bases_pending {
            if let Some(b) = &bases {
                print_bases(&mut msg, b);
                bases_pending = false;
            }
        }
        // With `--attach`/`--inline` the trailing MIME boundary replaces the
        // `-- \n<version>` signature entirely.
        match &opts.mime_boundary {
            Some(b) => {
                write!(msg, "\n--{MIME_BOUNDARY_LEADER}{b}--\n\n\n")?;
            }
            None => write_signature(&mut msg, &opts),
        }

        // git puts one extra blank line between patches in the mbox stream; the
        // cover letter is not separated that way.
        if opts.to_stdout && idx > 0 {
            // `show_log()`'s inter-record newline (log-tree.c:770-790), which
            // `graph_show_padding()` opens with `--line-prefix` for a NULL graph.
            if let Some(p) = &opts.line_prefix {
                buffered.extend_from_slice(p.as_bytes());
            }
            buffered.push(b'\n');
        }
        emit_message(&mut buffered, &msg, patch_filename(&commit, nr, &opts)?, &opts)?;
    }

    match stdout.write_all(&buffered).and_then(|()| stdout.flush()) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        // A downstream `| head` closing the pipe is not an error; git leaves by
        // way of SIGPIPE rather than returning a status of its own.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
            crate::sigpipe::exit_broken_pipe()
        }
        Err(e) => Err(e.into()),
    }
}

/// Port of `diff_title()` (`builtin/log.c`): a reroll count of at least 1 names
/// the version this series is being compared against, `v<n-1>`.
fn diff_title(reroll: Option<&str>, generic: &str, rerolled: &str) -> String {
    match reroll.and_then(strtol_i) {
        Some(v) if v >= 1 => format!("{rerolled}{}:", v - 1),
        _ => generic.to_string(),
    }
}

/// Port of `infer_range_diff_ranges()` (`builtin/log.c`): the two ranges
/// `--range-diff=<prev>` stands for.
///
/// `prev` is either already a range — in which case it is the left side
/// verbatim — or a single tip, which becomes `<head>..<prev>`. The right side
/// runs from the series' origin to its tip; without an origin git falls back to
/// `prev` and says so, or gives up when `prev` was a range.
fn infer_range_diff_ranges(
    repo: &gix::Repository,
    prev: &str,
    commits: &[ObjectId],
) -> Result<(String, String)> {
    let head = commits.last().expect("caller rejects an empty series");
    let head_hex = head.to_hex().to_string();
    let prev_is_range = is_range_diff_range(repo, prev);
    let r1 = if prev_is_range {
        prev.to_string()
    } else {
        format!("{head_hex}..{prev}")
    };
    let r2 = match series_origin(repo, commits)? {
        Some(origin) => format!("{}..{head_hex}", origin.to_hex()),
        None if prev_is_range => {
            crate::git_fatal!("failed to infer range-diff origin of current series")
        }
        None => {
            eprintln!("warning: using '{prev}' as range-diff origin of current series");
            format!("{prev}..{head_hex}")
        }
    };
    Ok((r1, r2))
}

/// Port of `is_range_diff_range()` (`range-diff.c`): true when the argument
/// resolves to both a positive and a negative endpoint, i.e. it names a range
/// rather than a single tip.
fn is_range_diff_range(repo: &gix::Repository, arg: &str) -> bool {
    let resolves = |s: &str| {
        repo.rev_parse_single(BStr::new(if s.is_empty() { "HEAD" } else { s }))
            .is_ok()
    };
    // Only the two range spellings contribute endpoints of both signs: a bare
    // tip is positive-only and a `^<rev>` is negative-only, so neither counts.
    let Some((left, right)) = arg.split_once("...").or_else(|| arg.split_once("..")) else {
        return false;
    };
    resolves(left) && resolves(right)
}

/// The `origin` `cmd_format_patch` picks up from its walk: the single boundary
/// commit of the series, i.e. the one commit that a listed commit descends from
/// while not being listed itself. git sets `origin` only while exactly one
/// boundary commit has been seen (`origin = (boundary_count == 1) ? commit :
/// NULL`), so a series with several roots or several bases has none.
fn series_origin(repo: &gix::Repository, commits: &[ObjectId]) -> Result<Option<ObjectId>> {
    let listed: std::collections::HashSet<ObjectId> = commits.iter().copied().collect();
    let mut boundary: Vec<ObjectId> = Vec::new();
    for id in commits {
        for parent in repo.find_commit(*id)?.parent_ids() {
            let parent = parent.detach();
            if !listed.contains(&parent) && !boundary.contains(&parent) {
                boundary.push(parent);
            }
        }
    }
    Ok(match boundary.len() {
        1 => Some(boundary[0]),
        _ => None,
    })
}

/// Port of `show_diff_of_diff()` (`log-tree.c`) and the tail of
/// `make_cover_letter()`: the `Interdiff:` block, then the `Range-diff:` block.
///
/// `indent` is git's `output_prefix` width for the interdiff — two spaces from
/// `show_diff_of_diff()`, none from the cover letter — and doubles as the flag
/// for the leading blank line the patch form prints and the cover letter does
/// not. The range-diff carries its own four-space indent internally and is never
/// given an output prefix, in either form.
fn emit_diff_of_diff(
    repo: &gix::Repository,
    opts: &Opts,
    ranges: Option<&(String, String)>,
    creation_factor: i64,
    commits: &[ObjectId],
    indent: usize,
    out: &mut Vec<u8>,
) -> Result<std::result::Result<(), ExitCode>> {
    if let Some(oid1) = opts.interdiff.last() {
        let head = commits.last().expect("caller rejects an empty series");
        let new_tree = repo.find_object(*head)?.peel_to_tree()?;
        let old_tree = repo.find_object(*oid1)?.peel_to_tree()?;
        if indent > 0 {
            out.push(b'\n');
        }
        writeln!(out, "{}", diff_title(opts.reroll.as_deref(), "Interdiff:", "Interdiff against v"))?;
        let mut body: Vec<u8> = Vec::new();
        let abbrev = index_abbrev(repo, &new_tree, opts)?;
        let mut dissimilarity = HashMap::new();
        let changes = tree_changes(
            repo,
            Some(&old_tree),
            Some(&new_tree),
            opts.pathspec.as_ref(),
            opts.active_relative(),
            &RenameOpts::from_opts(opts),
            &mut dissimilarity,
        )?;
        for change in &changes {
            emit_change(repo, &mut body, change, abbrev, opts, &dissimilarity)?;
        }
        write_indented(out, &body, indent);
    }

    if let Some((r1, r2)) = ranges {
        if indent > 0 {
            out.push(b'\n');
        }
        writeln!(out, "{}", diff_title(opts.reroll.as_deref(), "Range-diff:", "Range-diff against v"))?;
        // `get_notes_args()`: format-patch pushes `--no-notes` unless the series
        // itself renders notes (`rev->show_notes`).
        let notes_on = opts.notes.show;
        match super::range_diff::show_range_diff(
            repo,
            r1,
            r2,
            creation_factor,
            notes_on,
            out,
        )? {
            Ok(()) => {}
            Err(code) => return Ok(Err(code)),
        }
    }
    Ok(Ok(()))
}

/// git's `output_prefix`: every emitted line of a diff carries it, so a rendered
/// patch is indented by prefixing each of its lines.
fn write_indented(out: &mut Vec<u8>, body: &[u8], indent: usize) {
    if indent == 0 {
        out.extend_from_slice(body);
        return;
    }
    for line in body.split_inclusive(|&b| b == b'\n') {
        out.resize(out.len() + indent, b' ');
        out.extend_from_slice(line);
    }
}

/// Append one rendered message to the mbox stream, or write it to its file and
/// note the name for stdout.
fn emit_message(buffered: &mut Vec<u8>, msg: &[u8], name: String, opts: &Opts) -> Result<()> {
    // `--output=<file>`: every message of the series is appended to the one file the
    // option opened, and nothing is announced on stdout.
    if let Some(path) = &opts.output {
        let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
        f.write_all(msg)?;
        return Ok(());
    }
    if opts.to_stdout {
        buffered.extend_from_slice(msg);
        return Ok(());
    }
    let path = match &opts.outdir {
        Some(dir) => {
            std::fs::create_dir_all(dir)?;
            format!("{dir}/{name}")
        }
        None => name.clone(),
    };
    if !opts.quiet {
        let shown = match &opts.outdir {
            Some(_) => path.clone(),
            None => name,
        };
        writeln!(buffered, "{shown}")?;
    }
    std::fs::write(&path, msg)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

enum Parsed {
    Ready(Box<Opts>),
    /// git refused the command line itself; the message is already on stderr.
    Exit(ExitCode),
}

/// Flags whose effect is already this module's behavior, so accepting them
/// changes nothing: progress is never rendered, an external diff or textconv
/// driver is never run, and format-patch never diffs the index, so an
/// intent-to-add entry cannot reach it.
const NO_OP: &[&str] = &[
    // `rev.always_show_header`, which `format-patch` sets for every commit anyway:
    // measured against stock 2.55.0 over single commits, ranges, and commits whose
    // diff is empty, the output is identical with and without it.
    "--always",
    "--no-textconv",
    "--no-ext-diff",
    "--progress",
    "--no-progress",
    // format-patch compares two *trees*; there is no index in the comparison, so
    // neither spelling of the intent-to-add rule can reach anything.
    "--ita-invisible-in-index",
    "--ita-visible-in-index",
    // `cmd_format_patch()` ends `return 0` on every path, so
    // `diff_options.flags.exit_with_status` has no reader here. Measured against
    // stock 2.55.0: `--exit-code` exits 0 both over a commit with a diff and over
    // an empty `HEAD..HEAD`.
    "--exit-code",
    // The pickaxe kinds these two modify are `-S`, `-G` and `--find-object`, all
    // of which this module still refuses — so on a command line it accepts, there
    // is no pickaxe for them to act on. Implementing `-S`/`-G` means revisiting
    // these two at the same time.
    "--pickaxe-all",
    "--pickaxe-regex",
];

/// Flags git accepts that this module has not ported. Matched as `--flag` or
/// `--flag=<value>`; see the module header for what each of them would change.
const DEFERRED: &[&str] = &[
    "--ignore-if-in-upstream",
    // `--textconv`/`--ext-diff` need the `gitattributes` diff-driver plumbing that
    // `git diff` reaches through the vendored filter stack; nothing in this module
    // can run a driver yet.
    "--textconv",
    "--ext-diff",
    // `--anchored` is xdiff's `xpp->anchors`, carried into the patience pass
    // (`xdiff/xpatience.c:74-76`). The vendored `gix-imara-diff` has the patience
    // algorithm but omits every anchor branch — its `patience.rs:21-22` records that
    // `is_anchor()` is constantly false — so the anchors would be silently dropped.
    "--anchored",
];

/// Short diff options whose value is *required*, so `parse_short_opt()` takes it
/// from the next argv slot when nothing is attached: `-S <string>` is `-S<string>`.
/// Measured against stock 2.55.0 — `git format-patch -S 5 --stdout -1` prints the
/// last commit's patch (the `5` was eaten), while `-M 5`, whose value is only ever
/// attached (`PARSE_OPT_OPTARG`), dies `ambiguous argument '5'` on the `5`.
const DEFERRED_SHORT_VALUE: &[&str] = &["-O", "-S", "-G"];

/// `builtin_format_patch_options` entries that need a value in the next argv
/// slot (or attached after `=` / directly after the short letter).
const FP_VALUE_OPTS: &[&str] = &[
    "--commit-list-format",
    "--suffix",
    "--start-number",
    "--reroll-count",
    "--filename-max-length",
    "--cover-from-description",
    "--description-file",
    "--subject-prefix",
    "--output-directory",
    "--add-header",
    "--to",
    "--cc",
    "--in-reply-to",
    "--signature",
    "--base",
    "--signature-file",
    "--interdiff",
    "--range-diff",
    "--creation-factor",
];

/// `PARSE_OPT_OPTARG` entries: a value is allowed but only attached, so the next
/// argv slot is never theirs.
const FP_OPTARG_OPTS: &[&str] = &["--rfc", "--from", "--attach", "--inline", "--thread"];

/// The valueless entries, `--no-` spellings that stand on their own included.
const FP_FLAG_OPTS: &[&str] = &[
    "--numbered",
    "--no-numbered",
    "--signoff",
    "--no-signoff",
    "--stdout",
    "--no-stdout",
    "--cover-letter",
    "--no-cover-letter",
    "--numbered-files",
    "--no-numbered-files",
    "--keep-subject",
    "--binary",
    "--no-binary",
    "--zero-commit",
    "--no-zero-commit",
    "--ignore-if-in-upstream",
    "--no-ignore-if-in-upstream",
    "--no-stat",
    "--quiet",
    "--no-quiet",
    "--progress",
    "--no-progress",
    "--force-in-body-from",
    "--no-force-in-body-from",
];

/// How many argv slots `parse_options()` takes for `arg` against
/// `builtin_format_patch_options`, or `None` when the option is not in that
/// table at all.
///
/// `cmd_format_patch` parses with `PARSE_OPT_KEEP_UNKNOWN_OPT`, so anything this
/// returns `None` for stays in argv and reaches `setup_revisions()`. That is the
/// only distinction that matters here: `verify_filename()` dies on whatever
/// `-`-prefixed word is *still* in argv once a positional turns out to name a
/// path, which is why `format-patch <path> --no-thread` is quietly accepted
/// while `format-patch <path> --always` is fatal.
fn fp_option_slots(arg: &str) -> Option<usize> {
    if let Some(rest) = arg.strip_prefix("--") {
        let (name, attached) = match rest.split_once('=') {
            Some((n, _)) => (n, true),
            None => (rest, false),
        };
        let long = format!("--{name}");
        if FP_VALUE_OPTS.contains(&long.as_str()) {
            return Some(usize::from(!attached));
        }
        if FP_OPTARG_OPTS.contains(&long.as_str()) || FP_FLAG_OPTS.contains(&long.as_str()) {
            return Some(0);
        }
        // Negating a value-taking option never takes the value with it.
        if let Some(bare) = name.strip_prefix("no-") {
            let bare = format!("--{bare}");
            let known =
                FP_VALUE_OPTS.contains(&bare.as_str()) || FP_OPTARG_OPTS.contains(&bare.as_str());
            if known && !attached {
                return Some(0);
            }
        }
        return None;
    }
    // Short options, which `parse_short_opt()` reads one letter at a time out of
    // a cluster. An unknown letter abandons the whole word to argv, value letter
    // or not, so the loop reports `None` for `-n3` exactly as it does for `-3`.
    let rest = arg.strip_prefix('-').filter(|r| !r.is_empty())?;
    for (idx, c) in rest.char_indices() {
        match c {
            'n' | 'N' | 's' | 'k' | 'p' | 'q' => {}
            // `-v2`/`-oDIR` carry the value; a trailing `-v`/`-o` eats the next slot.
            'v' | 'o' => return Some(usize::from(idx + c.len_utf8() == rest.len())),
            _ => return None,
        }
    }
    Some(0)
}

/// The words `setup_revisions()` still has in argv from `at` onward — argv as
/// `parse_options()` handed it back, so format-patch's own options and their
/// values are gone.
fn revision_argv_from(argv: &[String], at: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut i = at;
    while i < argv.len() {
        match fp_option_slots(&argv[i]) {
            Some(extra) => i += 1 + extra,
            None => {
                out.push(argv[i].as_str());
                i += 1;
            }
        }
    }
    out
}

/// Port of the `for (j = i; j < argc; j++) verify_filename(...)` sweep
/// `setup_revisions()` runs the moment a word stops being a revision: from there
/// on every remaining word has to be a usable pathspec, and the first one that
/// is not decides the message.
///
/// Verified against stock git 2.55.0 in a worktree holding `README.md`:
/// `format-patch README.md --always` is
/// `fatal: option '--always' must come before non-option arguments`, while
/// `format-patch README.md HEAD` is `fatal: HEAD: no such path in the working
/// tree.` and `format-patch README.md --no-thread` — a format-patch option, so
/// gone from argv before the sweep — is accepted.
///
/// The per-word rule is [`crate::setup::verify_filename`]'s, shared with every
/// other verb that splits revisions from paths; the private
/// `looks_like_pathspec()`/`check_filename()` pair that used to stand here was a
/// second transcription of setup.c with no caller of its own. Only the *sweep* —
/// which word carries `diagnose_misspelt_rev` — belongs to this command.
///
/// Returns the `die()` text, which the caller prints only once it has confirmed
/// nothing earlier on the command line preempts it.
fn verify_filenames(repo: &gix::Repository, words: &[&str]) -> Option<String> {
    for (n, word) in words.iter().enumerate() {
        // `die_verify_filename()`: only the word that failed to be a revision
        // gets the "did you mean a revision?" wording.
        let Some(message) = crate::setup::verify_filename(word, n == 0) else {
            continue;
        };
        // `die_verify_filename()`'s last stop before that wording:
        //
        // ```c
        // maybe_die_on_misspelt_object_name(r, arg, prefix);
        // /* ... or fall back the most general message. */
        // die(_("ambiguous argument '%s': unknown revision or path not in the working tree. ..."
        // ```
        //
        // and that helper is `get_oid_with_context_1(r, name, GET_OID_ONLY_TO_DIE
        // | GET_OID_QUIETLY, …)` — a *second* resolution of the same word. It is
        // silent unless the word takes `get_oid_basic()`'s full-hex branch and a
        // ref answers to those 40 characters, in which case the ambiguity warning
        // is printed again: `GET_OID_QUIETLY` gates the later, unrelated warning
        // and not this one. So stock prints it twice for
        // `format-patch <40-hex-that-is-also-a-ref>^{commit}`, once from
        // `handle_revision_arg_1()` and once from here.
        //
        // Two words never get that far, so neither may warn: one starting with
        // `-`, which `verify_filename()` dies on before `die_verify_filename()`,
        // and short-form pathspec magic, which the C skips explicitly —
        // `if (!(arg[0] == ':' && !isalnum(arg[1])))`, with `arg[1]` the NUL of a
        // bare `:` counting as non-alnum.
        let mut bytes = word.bytes();
        let pathspec_magic = bytes.next() == Some(b':')
            && !bytes.next().is_some_and(|b| b.is_ascii_alphanumeric());
        if n == 0 && !word.starts_with('-') && !pathspec_magic {
            crate::objname::warn_ambiguous_refname(repo, word);
        }
        return Some(message);
    }
    None
}

/// The namespace `--branches`/`--tags`/`--remotes` iterate, as
/// `refs_for_each_branch_ref()` and friends spell it.
fn namespace(opt: &str) -> Option<&'static str> {
    match opt {
        "--branches" => Some("refs/heads/"),
        "--tags" => Some("refs/tags/"),
        "--remotes" => Some("refs/remotes/"),
        _ => None,
    }
}

/// Record one `--all`-family selector at the position it stands on the command
/// line, taking the `--exclude` patterns that were waiting for it.
///
/// `handle_revision_pseudo_opt()` ends every one of these arms with
/// `clear_ref_exclusions()`, so the patterns never carry over to the next
/// selector. A selector under `--not` puts UNINTERESTING objects on the pending
/// list, which is what `add_pending_object_with_path()` clears `no_walk` for.
fn push_ref_set(
    o: &mut Opts,
    excludes: &mut Vec<String>,
    negate: bool,
    prefix: Option<&'static str>,
    pattern: Option<String>,
    with_head: bool,
) {
    if negate {
        o.no_walk = false;
    }
    o.revs.push(RevWord::Refs(RefSet {
        prefix,
        pattern,
        with_head,
        excludes: std::mem::take(excludes),
        negate,
    }));
}

/// True when `arg` is exactly `name` or the `name=<value>` form.
/// The spellings [`MoveWordOpts::parse_flag`] claims, recognised ahead of the call
/// so the match arm can dispatch without running the parser twice (it mutates).
fn is_move_word_flag(s: &str) -> bool {
    matches!(
        s,
        "--color-moved" | "--no-color-moved" | "--no-color-moved-ws" | "--word-diff" | "--color-words"
    ) || s.starts_with("--color-moved=")
        || s.starts_with("--color-moved-ws=")
        || s.starts_with("--word-diff=")
        || s.starts_with("--word-diff-regex=")
        || s.starts_with("--color-words=")
}

fn is_flag(arg: &str, name: &str) -> bool {
    arg == name || arg.strip_prefix(name).is_some_and(|r| r.starts_with('='))
}

fn parse(repo: &gix::Repository, args: &[String]) -> Result<Parsed> {
    // git reads the `format.*` config as the defaults for its options; the CLI
    // flags below override scalars and append to the address/header lists.
    let snap = repo.config_snapshot();
    let cfg_str = |k: &str| snap.string(k).and_then(|v| v.to_str().ok().map(str::to_owned));
    let cfg_list = |k: &str| {
        snap.plumbing()
            .values::<gix::bstr::BString>(k)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| v.to_str().ok().map(str::to_owned))
            .collect::<Vec<String>>()
    };

    // `format.from` resolves to an identity right away, because a value that is
    // neither a boolean nor a parsable ident line is fatal (exit 128).
    let cfg_from = match cfg_str("format.from") {
        Some(v) => match maybe_bool(&v) {
            Some(true) => Some(committer_ident(repo)?),
            Some(false) => None,
            None => match split_ident_line(&v) {
                Some(id) => Some(id),
                None => return Ok(Parsed::Exit(fatal(&format!("invalid ident line: {v}")))),
            },
        },
        // A valueless `[format] from` is the implicit-true boolean.
        None if snap.boolean("format.from") == Some(true) => Some(committer_ident(repo)?),
        None => None,
    };
    let cfg_cover_from = match cfg_str("format.coverFromDescription") {
        Some(v) => match parse_cover_from_description(&v) {
            Some(m) => m,
            None => {
                return Ok(Parsed::Exit(fatal(&format!(
                    "{v}: invalid cover from description mode"
                ))))
            }
        },
        None => CoverFrom::Message,
    };

    let mut o = Opts {
        to_stdout: false,
        outdir: cfg_str("format.outputDirectory"),
        outdir_cli: false,
        numbered: snap.boolean("format.numbered"),
        start_number: 1,
        numbered_files: false,
        suffix: cfg_str("format.suffix").unwrap_or_else(|| ".patch".to_owned()),
        subject_prefix: cfg_str("format.subjectPrefix").unwrap_or_else(|| "PATCH".to_owned()),
        reroll: None,
        signature: String::new(),
        sig_cli: SigCli::Unset,
        sig_file_arg: None,
        cfg_signature: cfg_str("format.signature"),
        cfg_signature_file: cfg_str("format.signatureFile"),
        zero_commit: false,
        use_patch_format: false,
        output_format: 0,
        dirstat: Dirstat {
            by_line: false,
            by_file: false,
            cumulative: false,
            permille: DIRSTAT_PERMILLE_DEFAULT,
        },
        stat_width: 0,
        stat_name_width: 0,
        stat_graph_width: 0,
        stat_count: 0,
        quiet: false,
        name_max: snap
            .integer("format.filenameMaxLength")
            .filter(|n| *n > 0)
            .map(|n| n as usize)
            .unwrap_or(NAME_MAX_DEFAULT),
        // Decided after the walk, where `cmd_format_patch` decides it; the
        // config only supplies the state feeding that decision.
        cover_letter: false,
        cover_setting: match snap.string("format.coverLetter") {
            None => CoverSetting::Unset,
            Some(v) if v.to_str_lossy().eq_ignore_ascii_case("auto") => CoverSetting::Auto,
            Some(v) => match maybe_bool(&v.to_str_lossy()) {
                Some(true) => CoverSetting::On,
                _ => CoverSetting::Off,
            },
        },
        keep_subject: false,
        in_reply_to: None,
        to: cfg_list("format.to"),
        cc: cfg_list("format.cc"),
        add_header: cfg_list("format.headers"),
        encode_email_headers: snap.boolean("format.encodeEmailHeaders").unwrap_or(true),
        mboxrd: snap.boolean("format.mboxrd") == Some(true),
        pretty_mboxrd: false,
        noprefix: snap.boolean("format.noprefix") == Some(true),
        src_prefix: "a/".to_owned(),
        dst_prefix: "b/".to_owned(),
        signoff: snap.boolean("format.signOff") == Some(true),
        from: cfg_from,
        force_in_body_from: snap.boolean("format.forceInBodyFrom") == Some(true),
        thread: match cfg_str("format.thread") {
            Some(v) if v.eq_ignore_ascii_case("deep") => Thread::Deep,
            Some(v) if v.eq_ignore_ascii_case("shallow") => Thread::Shallow,
            Some(v) => match maybe_bool(&v) {
                Some(true) => Thread::Shallow,
                _ => Thread::Unset,
            },
            None if snap.boolean("format.thread") == Some(true) => Thread::Shallow,
            None => Thread::Unset,
        },
        // `format.attach`: a value is the boundary, an empty value turns it off,
        // and a valueless key means the version string.
        mime_boundary: match cfg_str("format.attach") {
            Some(v) if v.is_empty() => None,
            Some(v) => Some(v),
            None if snap.plumbing().string("format.attach").is_some() => {
                Some(SIGNATURE_VERSION.to_owned())
            }
            None => None,
        },
        no_inline: snap.plumbing().string("format.attach").is_some(),
        cover_from: cfg_cover_from,
        description_file: None,
        branch_name: None,
        notes: match cfg_str("format.notes") {
            Some(v) => match maybe_bool(&v) {
                Some(true) => {
                    let mut o = super::notes::DisplayOpt::default();
                    o.enable_default();
                    o
                }
                Some(false) => super::notes::DisplayOpt::default(),
                None => {
                    let mut o = super::notes::DisplayOpt::default();
                    o.enable_ref(&v);
                    o
                }
            },
            None if snap.boolean("format.notes") == Some(true) => {
                let mut o = super::notes::DisplayOpt::default();
                o.enable_default();
                o
            }
            None => super::notes::DisplayOpt::default(),
        },
        auto_base: match cfg_str("format.useAutoBase") {
            Some(v) if v.eq_ignore_ascii_case("whenAble") => AutoBase::WhenAble,
            Some(v) => match maybe_bool(&v) {
                Some(true) => AutoBase::Always,
                _ => AutoBase::Never,
            },
            None if snap.boolean("format.useAutoBase") == Some(true) => AutoBase::Always,
            None => AutoBase::Never,
        },
        base_commit: None,
        commit_list_format: cfg_str("format.commitListFormat"),
        root: false,
        max_count: None,
        skip: 0,
        reverse: false,
        min_parents: 0,
        // format-patch sets `rev.max_parents = 1`: merges never get a patch.
        max_parents: Some(1),
        first_parent: false,
        order: Order::Default,
        no_walk: false,
        cherry_pick: false,
        cherry_mark: false,
        left_only: false,
        right_only: false,
        unsorted_input: false,
        revs: Vec::new(),
        paths: Vec::new(),
        seen_dashdash: false,
        argv: args.to_vec(),
        pathspec: None,
        context: 3,
        algorithm: Algorithm::Myers,
        text: false,
        no_binary: false,
        ignore_regex: Vec::new(),
        function_context: false,
        indent_heuristic: true,
        ws: super::diff_pairs::Whitespace::Keep,
        full_index: false,
        irreversible_delete: false,
        skip_or_rotate: None,
        detect_rename: None,
        rename_score: 0,
        find_copies_harder: false,
        rename_empty: true,
        break_opt: -1,
        rename_limit: None,
        expand_tabs: 0,
        compact_summary: false,
        inter_hunk_ctx: 0,
        ignore_blank_lines: false,
        diff_filter: None,
        colors: DiffColors::disabled(),
        extra: ExtraPaint::default(),
        ws_error_highlight: diff_color::WSEH_NEW,
        ws_rule: diff_color::whitespace_rule_cfg(repo),
        submodule_format: super::diff::SubmoduleFormat::Short,
        diff_merges: DiffMerges::Off,
        line_prefix: None,
        // `diff.relative` seeds the very flag `--relative` sets
        // (`options->flags.relative_name = diff_relative`, diff.c:422-425); measured
        // against stock 2.55.0, `-c diff.relative=true format-patch` run from a
        // subdirectory narrows and shortens exactly as a bare `--relative` does.
        relative_name: snap.boolean("diff.relative") == Some(true),
        relative_prefix: super::diff::cwd_prefix(repo),
        indicators: (b'+', b'-', b' '),
        abbrev: None,
        name_only: false,
        name_status: false,
        check: false,
        remerge_diff: false,
        deferred: Vec::new(),
        range_diff: None,
        creation_factor: None,
        interdiff: Vec::new(),
        cover_letter_given: false,
        output: None,
        opt_error: None,
    };

    let mut i = 0;
    let mut pathspec_mode = false;
    // `handle_revision_pseudo_opt()`'s `*flags ^= UNINTERESTING | BOTTOM`: `--not`
    // reverses the sense of everything after it and toggles again at the next one.
    let mut negate = false;
    // `revs->ref_excludes`, which every `--all`-family selector consumes and then
    // empties with `clear_ref_exclusions()`.
    let mut ref_excludes: Vec<String> = Vec::new();
    // git stores `--cover-from-description`'s value as a plain string during
    // option parsing and only validates it *after* the whole command line is
    // parsed. So an inline value error earlier or later on the line (e.g. a
    // malformed `--start-number`, which parse-options rejects in place with
    // exit 129) must win over this option's own exit-128 rejection. Capture the
    // last value here (last-wins) and validate it once the loop is done.
    let mut cover_from_desc: Option<String> = None;
    // git increments an internal `subject_prefix` counter whenever
    // `--subject-prefix`/`--rfc` is given, and later `die()`s if `-k` is also
    // set. Track only that it was given, not its value.
    let mut subject_prefix_given = false;
    // `--commit-list-format` implies a cover letter, but only when the caller
    // did not spell `--cover-letter`/`--no-cover-letter` out.
    let mut commit_list_format_given = false;
    // `--color[=<when>]` / `--no-color`, plus the two flags that force colour on
    // outright (`--color-words`, `--word-diff=color`) by assigning
    // `options->use_color = GIT_COLOR_ALWAYS` (diff.c:5606, 5626). `None` is "no
    // flag given", which falls through to `color.diff`/`color.ui`.
    let mut color_when: Option<ColorWhen> = None;
    // `--color-moved`/`--color-moved-ws`/`--word-diff*`/`--color-words`, collected
    // as spelled and layered over their config defaults once the loop is done.
    let mut move_word = MoveWordOpts::default();
    // `--ws-error-highlight=<kinds>`; `None` falls back to `diff.wsErrorHighlight`.
    let mut ws_error_highlight: Option<u32> = None;
    // `log.diffMerges` (`diff_merges_config()`, diff-merges.c:100-109) replaces
    // `set_to_default`, i.e. what `-m` and `--diff-merges=on|m` mean. It does not
    // change what a merge gets when neither is given — measured against stock
    // 2.55.0, `-c log.diffMerges=first-parent format-patch` over a merge still
    // emits header-only. A value git cannot parse is `-1` from the config callback,
    // which leaves the default alone.
    let diff_merges_default = cfg_str("log.diffMerges")
        .and_then(|v| parse_diff_merges(&v, DiffMerges::Separate))
        .unwrap_or(DiffMerges::Separate);
    while i < args.len() {
        let a = args[i].as_str();
        if pathspec_mode {
            o.paths.push(a.to_owned());
            i += 1;
            continue;
        }
        // The value checks `diff_opt_parse`'s callbacks run. Unlike every other
        // caller of this module, `cmd_format_patch` reaches them through
        // `setup_revisions()`, which runs *after* its own `parse_options()` pass
        // and walks argv resolving revisions as it goes — so a bad revision
        // earlier on the command line dies first. Recorded with its position and
        // resolved against the revisions, like the other value errors here.
        if let Some(line) = super::diff_optval::reject(a) {
            record_opt_error(&mut o.opt_error, i, 129, line);
        }
        match a {
            "--" => {
                pathspec_mode = true;
                o.seen_dashdash = true;
            }
            // parse_options_step()'s `internal_help`, which `cmd_format_patch`
            // runs before `setup_revisions`: the block on stdout at 129.
            // `--help-all` is the same step's own `strcmp()` and renders
            // `USAGE_FULL`, identical here — no entry is `PARSE_OPT_HIDDEN`.
            "-h" | "--help-all" => return Ok(Parsed::Exit(super::show_usage(USAGE))),
            "--stdout" => o.to_stdout = true,
            "-o" | "--output-directory" => {
                i += 1;
                let dir = value_at(args, i, a)?;
                if let Err(code) = set_outdir(&mut o, dir) {
                    return Ok(Parsed::Exit(code));
                }
            }
            "-n" | "--numbered" => o.numbered = Some(true),
            "-N" | "--no-numbered" => o.numbered = Some(false),
            "--start-number" => {
                i += 1;
                match parse_start_number(&value_at(args, i, a)?) {
                    Ok(n) => o.start_number = n,
                    Err(code) => return Ok(Parsed::Exit(code)),
                }
            }
            "--numbered-files" => o.numbered_files = true,
            "--subject-prefix" => {
                i += 1;
                o.subject_prefix = value_at(args, i, a)?;
                subject_prefix_given = true;
            }
            "--suffix" => {
                i += 1;
                o.suffix = value_at(args, i, a)?;
            }
            "-v" | "--reroll-count" => {
                i += 1;
                o.reroll = Some(value_at(args, i, a)?);
            }
            "--signature" => {
                i += 1;
                o.sig_cli = SigCli::Value(value_at(args, i, a)?);
            }
            "--no-signature" => o.sig_cli = SigCli::No,
            "--zero-commit" => o.zero_commit = true,
            "--no-zero-commit" => o.zero_commit = false,
            "--encode-email-headers" => o.encode_email_headers = true,
            "--no-encode-email-headers" => o.encode_email_headers = false,
            "--no-prefix" => o.noprefix = true,
            "--no-binary" => o.no_binary = true,
            "--binary" => o.no_binary = false,
            "--default-prefix" => {
                o.noprefix = false;
                o.src_prefix = "a/".to_owned();
                o.dst_prefix = "b/".to_owned();
            }
            "-p" | "--no-stat" => o.use_patch_format = true,
            // Each of these ORs its own `DIFF_FORMAT_*` bit in, which is what
            // makes them *replace* format-patch's `DIFFSTAT|SUMMARY` default
            // rather than add to it.
            "--stat" => o.output_format |= FMT_DIFFSTAT,
            // The `OPT_BITOP` trio at diff.c:6056-6066. `--patch-with-raw` and
            // `--patch-with-stat` are `-p --raw` and `-p --stat`, and every one of
            // them makes `output_format` non-zero, which is what keeps
            // `cmd_format_patch`'s stat+summary default from firing.
            // `diff_opt_compact_summary()` (diff.c:5657): the flag *and*
            // `DIFF_FORMAT_DIFFSTAT`, which is why `--compact-summary` alone also
            // suppresses format-patch's default summary block — `output_format` is
            // no longer zero, so the `DIFFSTAT | SUMMARY` default never fires.
            "--expand-tabs" => o.expand_tabs = 8,
            "--no-expand-tabs" => o.expand_tabs = 0,
            s if s.starts_with("--expand-tabs=") => {
                let v = &s["--expand-tabs=".len()..];
                match strtol_i(v) {
                    Some(n) if n >= 0 => o.expand_tabs = n as usize,
                    _ => return Ok(Parsed::Exit(fatal(&format!(
                        "'{v}': not a non-negative integer"
                    )))),
                }
            }
            "--compact-summary" => {
                o.compact_summary = true;
                o.output_format |= FMT_DIFFSTAT;
            }
            "--no-compact-summary" => o.compact_summary = false,
            "--raw" => o.output_format |= FMT_RAW,
            "--patch-with-raw" => {
                o.use_patch_format = true;
                o.output_format |= FMT_RAW;
            }
            "--patch-with-stat" => {
                o.use_patch_format = true;
                o.output_format |= FMT_DIFFSTAT;
            }
            "--summary" => o.output_format |= FMT_SUMMARY,
            "--numstat" => o.output_format |= FMT_NUMSTAT,
            "--shortstat" => o.output_format |= FMT_SHORTSTAT,
            "--cumulative" => {
                if let Err(code) = set_dirstat(&mut o, "cumulative") {
                    return Ok(Parsed::Exit(code));
                }
            }
            "-I" | "--ignore-matching-lines" => {
                i += 1;
                let pat = value_at(args, i, a)?;
                if let Err(code) = push_ignore_regex(&mut o, &pat) {
                    return Ok(Parsed::Exit(code));
                }
            }
            "--root" => o.root = true,
            "-q" | "--quiet" => o.quiet = true,
            "--filename-max-length" => {
                i += 1;
                match parse_filename_max_length(&value_at(args, i, a)?) {
                    Ok(n) => o.name_max = n,
                    Err(code) => return Ok(Parsed::Exit(code)),
                }
            }
            "--cover-letter" => {
                o.cover_letter = true;
                o.cover_letter_given = true;
            }
            "--no-cover-letter" => {
                o.cover_letter = false;
                o.cover_letter_given = true;
            }
            "-k" | "--keep-subject" => o.keep_subject = true,
            "--to" => {
                i += 1;
                o.to.push(value_at(args, i, a)?);
            }
            s if s.starts_with("--to=") => o.to.push(s["--to=".len()..].to_owned()),
            "--cc" => {
                i += 1;
                o.cc.push(value_at(args, i, a)?);
            }
            s if s.starts_with("--cc=") => o.cc.push(s["--cc=".len()..].to_owned()),
            "--add-header" => {
                i += 1;
                o.add_header.push(value_at(args, i, a)?);
            }
            s if s.starts_with("--add-header=") => {
                o.add_header.push(s["--add-header=".len()..].to_owned());
            }
            "--in-reply-to" => {
                i += 1;
                let v = value_at(args, i, a)?;
                match clean_message_id(&v) {
                    Some(id) => o.in_reply_to = Some(id),
                    None => return Ok(Parsed::Exit(fatal(&format!("insane in-reply-to: {v}")))),
                }
            }
            s if s.starts_with("--in-reply-to=") => {
                let v = &s["--in-reply-to=".len()..];
                match clean_message_id(v) {
                    Some(id) => o.in_reply_to = Some(id),
                    None => return Ok(Parsed::Exit(fatal(&format!("insane in-reply-to: {v}")))),
                }
            }
            "--signature-file" => {
                i += 1;
                o.sig_file_arg = Some(value_at(args, i, a)?);
            }
            s if s.starts_with("--signature-file=") => {
                o.sig_file_arg = Some(s["--signature-file=".len()..].to_owned());
            }
            "--rfc" => {
                o.subject_prefix = "RFC PATCH".to_owned();
                subject_prefix_given = true;
            }
            "-s" | "--signoff" => o.signoff = true,
            "--no-signoff" => o.signoff = false,
            "--force-in-body-from" => o.force_in_body_from = true,
            "--no-force-in-body-from" => o.force_in_body_from = false,
            // `--from` takes an *optional* value, so only the attached form
            // carries one; a bare `--from` means the committer identity.
            "--from" => o.from = Some(committer_ident(repo)?),
            "--no-from" => o.from = None,
            s if s.starts_with("--from=") => match split_ident_line(&s["--from=".len()..]) {
                Some(id) => o.from = Some(id),
                None => {
                    return Ok(Parsed::Exit(fatal(&format!(
                        "invalid ident line: {}",
                        &s["--from=".len()..]
                    ))))
                }
            },
            "--thread" => o.thread = Thread::Shallow,
            "--no-thread" => o.thread = Thread::Unset,
            // `--attach`/`--inline` differ only in the `Content-Disposition:`
            // they give the patch part; both default the boundary to git's
            // version string.
            "--attach" => {
                o.mime_boundary = Some(SIGNATURE_VERSION.to_owned());
                o.no_inline = true;
            }
            s if s.starts_with("--attach=") => {
                o.mime_boundary = Some(s["--attach=".len()..].to_owned());
                o.no_inline = true;
            }
            "--no-attach" => {
                o.mime_boundary = None;
                o.no_inline = false;
            }
            "--inline" => {
                o.mime_boundary = Some(SIGNATURE_VERSION.to_owned());
                o.no_inline = false;
            }
            s if s.starts_with("--inline=") => {
                o.mime_boundary = Some(s["--inline=".len()..].to_owned());
                o.no_inline = false;
            }
            "--cover-from-description" => {
                i += 1;
                cover_from_desc = Some(value_at(args, i, a)?);
            }
            "--description-file" => {
                i += 1;
                o.description_file = Some(value_at(args, i, a)?);
            }
            s if s.starts_with("--description-file=") => {
                o.description_file = Some(s["--description-file=".len()..].to_owned());
            }
            "--commit-list-format" => {
                i += 1;
                o.commit_list_format = Some(value_at(args, i, a)?);
                commit_list_format_given = true;
            }
            s if s.starts_with("--commit-list-format=") => {
                o.commit_list_format = Some(s["--commit-list-format=".len()..].to_owned());
                commit_list_format_given = true;
            }
            // `--notes` takes an optional ref; `--no-notes` clears everything.
            "--notes" => o.notes.enable_default(),
            s if s.starts_with("--notes=") => o.notes.enable_ref(&s["--notes=".len()..]),
            "--no-notes" => o.notes.disable(),
            // `--base` is git's `base_callback`: `auto` switches on the
            // upstream-derived base, any other value names the base itself.
            "--base" => {
                i += 1;
                set_base(&mut o, &value_at(args, i, a)?);
            }
            s if s.starts_with("--base=") => set_base(&mut o, &s["--base=".len()..]),
            "--no-base" => {
                o.auto_base = AutoBase::Never;
                o.base_commit = None;
            }
            "--reverse" => o.reverse = true,
            // The three ordering flags are the same option in `revision.c`: each
            // sets `topo_order` and differs only in the `sort_order` tie-break, so
            // the last one on the command line wins.
            "--topo-order" => o.order = Order::Topo,
            "--date-order" => o.order = Order::DateTopo,
            "--author-date-order" => o.order = Order::AuthorDateTopo,
            "--no-walk" => o.no_walk = true,
            s if s.starts_with("--no-walk=") => {
                match &s["--no-walk=".len()..] {
                    "sorted" => o.unsorted_input = false,
                    "unsorted" => o.unsorted_input = true,
                    // `handle_revision_pseudo_opt()` reports the value and returns
                    // an error, which `setup_revisions()` does *not* treat as
                    // fatal: the argument falls through to the unknown-option list
                    // and `cmd_format_patch` then rejects it as unrecognized.
                    _ => {
                        eprintln!("error: invalid argument to --no-walk");
                        return Ok(Parsed::Exit(fatal(&format!(
                            "unrecognized argument: {s}"
                        ))));
                    }
                }
                o.no_walk = true;
            }
            "--do-walk" => o.no_walk = false,
            "--no-merges" => o.max_parents = Some(1),
            // git has no `--no-first-parent`; the flag is one-way.
            "--first-parent" => o.first_parent = true,
            // The three bare spellings are `diff_opt_diff_algorithm()` with the
            // option's own name as the value (diff.c:5689-5704), so they set the
            // same knob `--diff-algorithm=<name>` does and the last one on the
            // line wins.
            "--minimal" => o.algorithm = Algorithm::MyersMinimal,
            "--patience" => o.algorithm = Algorithm::Patience,
            "--histogram" => o.algorithm = Algorithm::Histogram,
            "-a" | "--text" => o.text = true,
            s if s.starts_with("--output-directory=") => {
                let dir = s["--output-directory=".len()..].to_owned();
                if let Err(code) = set_outdir(&mut o, dir) {
                    return Ok(Parsed::Exit(code));
                }
            }
            s if s.starts_with("--start-number=") => {
                match parse_start_number(&s["--start-number=".len()..]) {
                    Ok(n) => o.start_number = n,
                    Err(code) => return Ok(Parsed::Exit(code)),
                }
            }
            s if s.starts_with("--subject-prefix=") => {
                o.subject_prefix = s["--subject-prefix=".len()..].to_owned();
                subject_prefix_given = true;
            }
            s if s.starts_with("--suffix=") => o.suffix = s["--suffix=".len()..].to_owned(),
            s if s.starts_with("--reroll-count=") => {
                o.reroll = Some(s["--reroll-count=".len()..].to_owned());
            }
            s if s.starts_with("--signature=") => {
                o.sig_cli = SigCli::Value(s["--signature=".len()..].to_owned());
            }
            s if s.starts_with("--filename-max-length=") => {
                match parse_filename_max_length(&s["--filename-max-length=".len()..]) {
                    Ok(n) => o.name_max = n,
                    Err(code) => return Ok(Parsed::Exit(code)),
                }
            }
            s if s.starts_with("--rfc=") => {
                o.subject_prefix = format!("{} PATCH", &s["--rfc=".len()..]);
                subject_prefix_given = true;
            }
            // The revision-walk counts share git's strict signed-int parser
            // (`strtol_i`, base 10): trailing junk or a non-numeral is
            // `die("'%s': not an integer")` (exit 128) from inside
            // setup_revisions, so it is recorded positionally rather than
            // emitted in place. A negative value disables the corresponding
            // bound the way revision.c's `>= 0` guards do.
            s if s.starts_with("--max-count=") => {
                let val = &s["--max-count=".len()..];
                match strtol_i(val) {
                    Some(v) => o.max_count = (v >= 0).then_some(v as usize),
                    None => not_an_integer(&mut o.opt_error, i, val),
                }
                // `handle_revision_opt()` clears `no_walk` for every spelling of
                // the commit count: asking for the newest <n> is asking to walk.
                o.no_walk = false;
            }
            s if s.starts_with("--skip=") => {
                let val = &s["--skip=".len()..];
                match strtol_i(val) {
                    Some(v) => o.skip = v.max(0) as usize,
                    None => not_an_integer(&mut o.opt_error, i, val),
                }
            }
            s if s.starts_with("--min-parents=") => {
                let val = &s["--min-parents=".len()..];
                match strtol_i(val) {
                    Some(v) => o.min_parents = v.max(0) as usize,
                    None => not_an_integer(&mut o.opt_error, i, val),
                }
            }
            s if s.starts_with("--max-parents=") => {
                let val = &s["--max-parents=".len()..];
                match strtol_i(val) {
                    Some(v) => o.max_parents = (v >= 0).then_some(v as usize),
                    None => not_an_integer(&mut o.opt_error, i, val),
                }
            }
            s if s.starts_with("--unified=") => {
                o.context = parse_num(&s["--unified=".len()..])? as u32;
            }
            s if s.len() > 2 && s.starts_with("-U") && s[2..].bytes().all(|c| c.is_ascii_digit()) => {
                o.context = parse_num(&s[2..])? as u32;
            }
            // `diff_opt_diff_algorithm()` hands the value to
            // `parse_algorithm_value()` (diff.c:220-236), which compares with
            // `strcasecmp` and takes `default` as another spelling of `myers`.
            // The accept set and the rejection message are
            // [`crate::diffopt::check`]'s, shared with every other command built
            // from `add_diff_options()` — the private copy that used to stand
            // here had drifted into a case-sensitive set without `default`.
            s if s.starts_with("--diff-algorithm=") => {
                let value = &s["--diff-algorithm=".len()..];
                match crate::diffopt::check("diff-algorithm", Some(value)) {
                    // Every name git accepts maps onto an algorithm the vendored
                    // `gix-imara-diff` implements, `patience` included: its
                    // `patience.rs` is a port of git's `xdiff/xpatience.c` and
                    // `Algorithm::Patience` is wired through `diff()`
                    // (gix-imara-diff/src/lib.rs:214,298).
                    Ok(()) => {
                        o.algorithm = match value.to_ascii_lowercase().as_str() {
                            "minimal" => Algorithm::MyersMinimal,
                            "patience" => Algorithm::Patience,
                            "histogram" => Algorithm::Histogram,
                            // `myers` and `default`, the only names left.
                            _ => Algorithm::Myers,
                        }
                    }
                    // git's `parse_algorithm_value()` rejects this in
                    // setup_revisions (exit 129); recorded positionally so an
                    // earlier bad revision preempts it.
                    Err(message) => {
                        record_opt_error(&mut o.opt_error, i, 129, format!("error: {message}"))
                    }
                }
            }
            s if s.starts_with("--thread=") => match &s["--thread=".len()..] {
                "shallow" => o.thread = Thread::Shallow,
                "deep" => o.thread = Thread::Deep,
                // git rejects the value with a bare usage exit and no message.
                _ => return Ok(Parsed::Exit(ExitCode::from(129))),
            },
            s if s.starts_with("--cover-from-description=") => {
                cover_from_desc = Some(s["--cover-from-description=".len()..].to_owned());
            }
            // git's `parse_ignore_submodules_arg()` accepts only these four
            // words; anything else is `die("bad --ignore-submodules argument")`
            // (exit 128) from setup_revisions. "none" is already this module's
            // behavior (submodule changes are shown); the other three only affect
            // an unported render, so they are deferred.
            s if s.starts_with("--ignore-submodules=") => {
                match &s["--ignore-submodules=".len()..] {
                    "none" => {}
                    "all" | "untracked" | "dirty" => o.deferred.push(a.to_owned()),
                    v => record_opt_error(
                        &mut o.opt_error,
                        i,
                        128,
                        format!("fatal: bad --ignore-submodules argument: {v}"),
                    ),
                }
            }
            // `diff_opt_diff_filter()` (diff.c:5470-5500). Every occurrence ors into
            // the same two bit sets, and an unknown letter is `error()` from inside
            // setup_revisions, so an earlier bad revision preempts it.
            s if s.starts_with("--diff-filter=") => {
                let value = &s["--diff-filter=".len()..];
                let mut filter = o.diff_filter.unwrap_or_default();
                match filter.accumulate(value) {
                    Ok(()) => o.diff_filter = Some(filter),
                    Err(ch) => record_opt_error(
                        &mut o.opt_error,
                        i,
                        129,
                        format!("error: unknown change class '{ch}' in --diff-filter={value}"),
                    ),
                }
            }
            // `OPT_UNSIGNED(0, "inter-hunk-context", …)` (diff.c:6144-6145): a
            // non-negative integer with an optional k/m/g suffix, rejected at exit 129
            // by parse-options' own `OPT_UNSIGNED` diagnostic.
            s if s.starts_with("--inter-hunk-context=") || s == "--inter-hunk-context" => {
                let value = if s == "--inter-hunk-context" {
                    i += 1;
                    match args.get(i) {
                        Some(v) => v.clone(),
                        // parse-options' own `opterror(..., "requires a value")`,
                        // emitted in place at exit 129.
                        None => {
                            eprintln!("error: {}", diff_color::missing_value(s));
                            return Ok(Parsed::Exit(ExitCode::from(129)));
                        }
                    }
                } else {
                    s["--inter-hunk-context=".len()..].to_owned()
                };
                match crate::optint::unsigned(
                    &crate::optint::long_opt("inter-hunk-context"),
                    &value,
                ) {
                    Ok(n) => o.inter_hunk_ctx = n as i64,
                    Err(e) => record_opt_error(&mut o.opt_error, i, 129, format!("error: {e}")),
                }
            }
            // `OPT_BIT_F(0, "ignore-blank-lines", …, XDF_IGNORE_BLANK_LINES,
            // PARSE_OPT_NONEG)` (diff.c:6208-6210): no `--no-` spelling exists.
            "--ignore-blank-lines" => o.ignore_blank_lines = true,
            // `diff_opt_line_prefix()` (diff.c:5788-5795), an `OPT_CALLBACK_F` with a
            // required argument, so a bare `--line-prefix` takes the next argv slot.
            "--line-prefix" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("error: {}", diff_color::missing_value(a));
                    return Ok(Parsed::Exit(ExitCode::from(129)));
                };
                o.line_prefix = Some(v.clone());
            }
            s if s.starts_with("--line-prefix=") => {
                o.line_prefix = Some(s["--line-prefix=".len()..].to_owned());
            }
            // `diff_opt_relative()` (diff.c:5750): with no value the prefix is the
            // current directory inside the repository, with one it is that path, and
            // either way it is stored with a trailing slash.
            "--relative" => o.relative_name = true,
            "--no-relative" => o.relative_name = false,
            s if s.starts_with("--relative=") => {
                o.relative_name = true;
                o.relative_prefix = s["--relative=".len()..].to_string();
            }
            // `diff_opt_submodule()` (diff.c:5804): `PARSE_OPT_OPTARG`, so a bare
            // `--submodule` is `log`, and an unparsable value is `error()` at 129.
            // `diff.submodule` is *not* consulted — measured against stock 2.55.0,
            // `-c diff.submodule=log format-patch` still renders the `short` form,
            // while the same value does move `git diff`.
            "--submodule" => o.submodule_format = super::diff::SubmoduleFormat::Log,
            s if s.starts_with("--submodule=") => {
                let value = &s["--submodule=".len()..];
                match super::diff::parse_submodule_params(value) {
                    Some(f) => o.submodule_format = f,
                    None => record_opt_error(
                        &mut o.opt_error,
                        i,
                        129,
                        format!("error: failed to parse --submodule option parameter: '{value}'"),
                    ),
                }
            }
            // `OPT_SET_INT('W', "function-context", …)`, which sets
            // `XDL_EMIT_FUNCCONTEXT` on the blob diffs.
            "-W" | "--function-context" => o.function_context = true,
            "--no-function-context" => o.function_context = false,
            // `OPT_CALLBACK(0, "interdiff", &idiff_prev, …, parse_opt_object_name)`:
            // every occurrence appends to an oid array and `--no-interdiff` clears
            // it; only the last entry is used. The name is resolved here, as the
            // callback does, so a name that resolves to nothing is a parse-time
            // error ahead of the walk.
            "--interdiff" => {
                i += 1;
                let v = value_at(args, i, a)?;
                if let Err(code) = push_interdiff(repo, &mut o, &v) {
                    return Ok(Parsed::Exit(code));
                }
            }
            s if s.starts_with("--interdiff=") => {
                let v = s["--interdiff=".len()..].to_owned();
                if let Err(code) = push_interdiff(repo, &mut o, &v) {
                    return Ok(Parsed::Exit(code));
                }
            }
            "--no-interdiff" => o.interdiff.clear(),
            // `OPT_INTEGER(0, "creation-factor", &creation_factor, …)`, whose
            // value parse-options rejects in place with its two distinct
            // messages: one for an empty value, one for a malformed one.
            "--creation-factor" => {
                i += 1;
                let v = value_at(args, i, a)?;
                set_creation_factor(&mut o, i, &v);
            }
            s if s.starts_with("--creation-factor=") => {
                let v = s["--creation-factor=".len()..].to_owned();
                set_creation_factor(&mut o, i, &v);
            }
            "--no-creation-factor" => o.creation_factor = None,
            // `--range-diff=<range>` / `--range-diff <range>`: the range is
            // validated after the walk (see `validate_range_diff`).
            "--range-diff" => {
                i += 1;
                o.range_diff = Some(value_at(args, i, a)?);
            }
            s if s.starts_with("--range-diff=") => {
                o.range_diff = Some(s["--range-diff=".len()..].to_owned());
            }
            // `OPT_FILENAME` opens the file as it parses, so it exists (empty) even when
            // a later check kills the command.
            "--output" => {
                i += 1;
                let v = value_at(args, i, a)?;
                std::fs::File::create(&v)?;
                o.output = Some(v);
            }
            s if s.starts_with("--output=") => {
                let v = s["--output=".len()..].to_owned();
                std::fs::File::create(&v)?;
                o.output = Some(v);
            }
            s if s.starts_with("--src-prefix=") => {
                o.src_prefix = s["--src-prefix=".len()..].to_owned();
                o.noprefix = false;
            }
            s if s.starts_with("--dst-prefix=") => {
                o.dst_prefix = s["--dst-prefix=".len()..].to_owned();
                o.noprefix = false;
            }
            // xdiff's `XDF_WHITESPACE_FLAGS`. Each is a plain assignment in
            // `diff_opt_parse()`, so the last one on the command line decides.
            "-w" | "--ignore-all-space" => o.ws = super::diff_pairs::Whitespace::IgnoreAll,
            "-b" | "--ignore-space-change" => o.ws = super::diff_pairs::Whitespace::IgnoreChange,
            "--ignore-space-at-eol" => o.ws = super::diff_pairs::Whitespace::IgnoreAtEol,
            "--ignore-cr-at-eol" => o.ws = super::diff_pairs::Whitespace::IgnoreCrAtEol,
            // `--full-index` pins the `index` line to the full object name;
            // `diff_setup_done()` lets it win over any `--abbrev` that was given.
            "--full-index" => o.full_index = true,
            "--no-full-index" => o.full_index = false,
            "-D" | "--irreversible-delete" => o.irreversible_delete = true,
            "--indent-heuristic" => o.indent_heuristic = true,
            "--no-indent-heuristic" => o.indent_heuristic = false,
            // `--rotate-to`/`--skip-to` share `diff_options::rotate_to` and differ
            // only in `skip_instead_of_rotate`, so whichever comes last wins.
            "--skip-to" | "--rotate-to" => {
                i += 1;
                let path = value_at(args, i, a)?;
                o.skip_or_rotate = Some((a == "--skip-to", path.into_bytes()));
            }
            s if s.starts_with("--skip-to=") => {
                o.skip_or_rotate = Some((true, s["--skip-to=".len()..].as_bytes().to_vec()));
            }
            s if s.starts_with("--rotate-to=") => {
                o.skip_or_rotate = Some((false, s["--rotate-to=".len()..].as_bytes().to_vec()));
            }
            // `diff_opt_color()` (diff.c:5591-5604): `PARSE_OPT_OPTARG`, so a bare
            // `--color` is `GIT_COLOR_ALWAYS` and a value goes through
            // `git_config_colorbool(NULL, arg)`, which takes only
            // never/always/auto — plus their boolean spellings — case-insensitively
            // and otherwise `error()`s (exit 129) from inside setup_revisions.
            "--color" => color_when = Some(ColorWhen::Always),
            "--no-color" => color_when = Some(ColorWhen::Never),
            s if s.starts_with("--color=") => {
                match diff_color::parse_color_when(&s["--color=".len()..]) {
                    Some(w) => color_when = Some(w),
                    None => record_opt_error(
                        &mut o.opt_error,
                        i,
                        129,
                        "error: option `color' expects \"always\", \"auto\", or \"never\""
                            .to_owned(),
                    ),
                }
            }
            // `--ws-error-highlight=<kinds>` (diff.c:6210-6212), whose
            // `parse_ws_error_highlight()` reports the offending suffix and exits 129
            // from setup_revisions.
            s if s.starts_with("--ws-error-highlight=") => {
                let arg = &s["--ws-error-highlight=".len()..];
                match diff_color::parse_ws_error_highlight(arg) {
                    Ok(v) => ws_error_highlight = Some(v),
                    // `%.*s` over the prefix git had already accepted (diff.c:5516).
                    Err(off) => record_opt_error(
                        &mut o.opt_error,
                        i,
                        129,
                        format!("error: unknown value after ws-error-highlight={}", &arg[..off]),
                    ),
                }
            }
            // `--stat=<width>[,<name-width>[,<count>]]`: git's `diff_opt_stat()`
            // parses each field with `strtoul(_, _, 10)`, keeping the previous
            // value for a field its comma never reaches, and rejects any leftover
            // (`error(_("invalid --stat value: %s"))`, exit 129) from inside
            // setup_revisions, so an earlier bad revision preempts it.
            s if s.starts_with("--stat=") => {
                let val = &s["--stat=".len()..];
                match parse_stat_value(val.as_bytes()) {
                    Some((width, name_width, count)) => {
                        o.stat_width = width;
                        if let Some(nw) = name_width {
                            o.stat_name_width = nw;
                        }
                        if let Some(c) = count {
                            o.stat_count = c;
                        }
                        o.output_format |= FMT_DIFFSTAT;
                    }
                    None => record_opt_error(
                        &mut o.opt_error,
                        i,
                        129,
                        format!("error: invalid --stat value: {val}"),
                    ),
                }
            }
            // The four scalar width knobs all route through `diff_opt_stat()`
            // too, each rejecting trailing junk with
            // `error(_("%s expects a numerical value"))` (exit 129) and each
            // OR-ing in `DIFF_FORMAT_DIFFSTAT`. The value is a required arg, so
            // the space-separated form consumes the next token.
            "--stat-width" | "--stat-name-width" | "--stat-graph-width" | "--stat-count" => {
                let flag = i;
                i += 1;
                let v = value_at(args, i, a)?;
                parse_stat_scalar(&mut o, &a[2..], &v, flag);
            }
            s if s.starts_with("--stat-width=") => {
                parse_stat_scalar(&mut o, "stat-width", &s["--stat-width=".len()..], i);
            }
            s if s.starts_with("--stat-name-width=") => {
                parse_stat_scalar(&mut o, "stat-name-width", &s["--stat-name-width=".len()..], i);
            }
            s if s.starts_with("--stat-graph-width=") => {
                parse_stat_scalar(
                    &mut o,
                    "stat-graph-width",
                    &s["--stat-graph-width=".len()..],
                    i,
                );
            }
            s if s.starts_with("--stat-count=") => {
                parse_stat_scalar(&mut o, "stat-count", &s["--stat-count=".len()..], i);
            }
            s if s.starts_with("--relative=") => {
                o.deferred.push(a.to_owned());
            }
            // `--dirstat`, `-X` and `--dirstat-by-file` all take an *optional*
            // value, so only the attached form carries parameters; a bare
            // `-X foo` leaves `foo` to be read as a revision, as git does.
            "--dirstat" | "-X" => {
                if let Err(code) = set_dirstat(&mut o, "") {
                    return Ok(Parsed::Exit(code));
                }
            }
            "--dirstat-by-file" => {
                if let Err(code) = set_dirstat(&mut o, "files") {
                    return Ok(Parsed::Exit(code));
                }
            }
            s if s.starts_with("--dirstat=") || s.starts_with("-X") => {
                let params = match s.strip_prefix("--dirstat=") {
                    Some(p) => p,
                    None => &s[2..],
                };
                if let Err(code) = set_dirstat(&mut o, params) {
                    return Ok(Parsed::Exit(code));
                }
            }
            s if s.starts_with("--dirstat-by-file=") => {
                if let Err(code) = set_dirstat(&mut o, "files") {
                    return Ok(Parsed::Exit(code));
                }
                if let Err(code) = set_dirstat(&mut o, &s["--dirstat-by-file=".len()..]) {
                    return Ok(Parsed::Exit(code));
                }
            }
            s if s.starts_with("--ignore-matching-lines=") => {
                let pat = s["--ignore-matching-lines=".len()..].to_owned();
                if let Err(code) = push_ignore_regex(&mut o, &pat) {
                    return Ok(Parsed::Exit(code));
                }
            }
            s if s.len() > 2 && s.starts_with("-I") => {
                let pat = s[2..].to_owned();
                if let Err(code) = push_ignore_regex(&mut o, &pat) {
                    return Ok(Parsed::Exit(code));
                }
            }
            s if s.len() > 2 && s.starts_with("-o") => {
                let dir = s[2..].to_owned();
                if let Err(code) = set_outdir(&mut o, dir) {
                    return Ok(Parsed::Exit(code));
                }
            }
            // `OPT_STRING('v', "reroll-count", …)`: git stores the value verbatim
            // and never checks it is a number — `-vabc` names the series `vabc`,
            // and only `diff_title()` later asks whether it parses as an integer.
            s if s.len() > 2 && s.starts_with("-v") => o.reroll = Some(s[2..].to_owned()),
            // `-<n>` is a commit count, unlike `-n` which means --numbered.
            s if s.len() > 1
                && s.starts_with('-')
                && s[1..].bytes().all(|c| c.is_ascii_digit()) =>
            {
                o.max_count = Some(parse_num(&s[1..])?);
                o.no_walk = false;
            }
            // The `--all` family. These are not format-patch options at all:
            // `parse_options()` keeps them (`PARSE_OPT_KEEP_UNKNOWN_OPT`) and
            // `handle_revision_pseudo_opt()` seeds them into the same pending list
            // the revision arguments feed, right where they stand — which is what
            // decides the walk's tie-break order, so each one is recorded in place.
            "--not" => negate = !negate,
            "--all" => push_ref_set(
                &mut o,
                &mut ref_excludes,
                negate,
                None,
                None,
                /* with_head */ true,
            ),
            "--branches" | "--tags" | "--remotes" => {
                push_ref_set(&mut o, &mut ref_excludes, negate, namespace(a), None, false);
            }
            s if matches!(
                s.split_once('='),
                Some(("--branches" | "--tags" | "--remotes", _))
            ) =>
            {
                let (name, pat) = s.split_once('=').expect("checked above");
                let pat = pat.to_owned();
                push_ref_set(&mut o, &mut ref_excludes, negate, namespace(name), Some(pat), false);
            }
            // `--glob`/`--exclude` take their value stuck or separate, since
            // `handle_revision_pseudo_opt()` reads them with `parse_long_opt()`
            // (diff.c) — which has its own wording for a missing value, and
            // `die()`s rather than going through parse-options' usage block.
            "--glob" | "--exclude" => {
                i += 1;
                let Some(value) = args.get(i).cloned() else {
                    return Ok(Parsed::Exit(fatal(&format!(
                        "Option '{a}' requires a value"
                    ))));
                };
                if a == "--exclude" {
                    ref_excludes.push(value);
                } else {
                    push_ref_set(&mut o, &mut ref_excludes, negate, None, Some(value), false);
                }
            }
            s if s.starts_with("--glob=") => {
                let pat = s["--glob=".len()..].to_owned();
                push_ref_set(&mut o, &mut ref_excludes, negate, None, Some(pat), false);
            }
            s if s.starts_with("--exclude=") => {
                ref_excludes.push(s["--exclude=".len()..].to_owned());
            }
            // The `Diff rename options` group (diff.c:6162-6189). `-M`/`-C`/`-B`
            // are `PARSE_OPT_OPTARG`, so a value is only ever attached; `-l` is
            // `OPT_INTEGER`, so it takes the next argv slot when nothing is.
            "--find-renames" | "-M" => set_rename_score(&mut o, i, "find-renames", ""),
            s if s.starts_with("--find-renames=") => {
                set_rename_score(&mut o, i, "find-renames", &s["--find-renames=".len()..]);
            }
            s if s.starts_with("-M") => set_rename_score(&mut o, i, "find-renames", &s[2..]),
            "--find-copies" | "-C" => set_rename_score(&mut o, i, "find-copies", ""),
            s if s.starts_with("--find-copies=") => {
                set_rename_score(&mut o, i, "find-copies", &s["--find-copies=".len()..]);
            }
            s if s.starts_with("-C") => set_rename_score(&mut o, i, "find-copies", &s[2..]),
            "--find-copies-harder" => o.find_copies_harder = true,
            "--no-find-copies-harder" => o.find_copies_harder = false,
            "--no-renames" => o.detect_rename = Some(0),
            "--rename-empty" => o.rename_empty = true,
            "--no-rename-empty" => o.rename_empty = false,
            "--break-rewrites" | "-B" => set_break_opt(&mut o, i, ""),
            s if s.starts_with("--break-rewrites=") => {
                set_break_opt(&mut o, i, &s["--break-rewrites=".len()..]);
            }
            s if s.starts_with("-B") => set_break_opt(&mut o, i, &s[2..]),
            "-l" => {
                i += 1;
                let Some(value) = args.get(i).cloned() else {
                    eprintln!("error: switch `l' requires a value");
                    return Ok(Parsed::Exit(ExitCode::from(129)));
                };
                set_rename_limit(&mut o, i, &value);
            }
            s if s.starts_with("-l") => set_rename_limit(&mut o, i, &s[2..]),
            // `diff_opt_char()` (diff.c): exactly one byte, or
            // `error(_("%s expects a character, got '%s'"))` at exit 129. An empty
            // value is not an error there — `arg[0]` is the terminator, so the
            // indicator becomes a NUL byte, which is what git then prints.
            "--output-indicator-new" | "--output-indicator-old" | "--output-indicator-context" => {
                i += 1;
                let Some(value) = args.get(i).cloned() else {
                    eprintln!("error: option `{}' requires a value", &a[2..]);
                    return Ok(Parsed::Exit(ExitCode::from(129)));
                };
                if let Err(code) = set_indicator(&mut o, i, &a[2..], &value) {
                    return Ok(Parsed::Exit(code));
                }
            }
            s if s.starts_with("--output-indicator-new=")
                || s.starts_with("--output-indicator-old=")
                || s.starts_with("--output-indicator-context=") =>
            {
                let (name, value) = s.split_once('=').expect("guarded by the pattern");
                if let Err(code) = set_indicator(&mut o, i, &name[2..], value) {
                    return Ok(Parsed::Exit(code));
                }
            }
            // `--abbrev[=<n>]` is `handle_revision_opt()`'s, not the diff option
            // table's: revision.c:2639-2648 sets `revs->abbrev` and revision.c:3172
            // copies it onto `diffopt.abbrev`. Bare `--abbrev` stores `DEFAULT_ABBREV`
            // and `--no-abbrev` stores 0, and `fill_metainfo()`'s
            // `abbrev = o->abbrev ? o->abbrev : DEFAULT_ABBREV` (diff.c:4915) turns
            // both back into the automatic length — measured identical to the default
            // under `core.abbrev=12`, where all three print twelve hex digits.
            "--abbrev" | "--no-abbrev" => o.abbrev = None,
            s if s.starts_with("--abbrev=") => {
                o.abbrev = Some(crate::abbrev::parse_abbrev_arg(
                    &s["--abbrev=".len()..],
                    repo.object_hash().len_in_hex(),
                ));
            }
            // builtin/log.c:2220-2227: format-patch has no output format but the
            // patch, so each of these four dies — but only after `setup_revisions()`
            // has resolved the revisions, so they are recorded here, not fatal here.
            "--name-only" | "--name-status" | "--check" => {
                if o.name_only || o.name_status || o.check {
                    // `diff_setup_done()` (diff.c:5259-5261) rejects two of the
                    // four output formats before `cmd_format_patch` gets to say
                    // which one it dislikes.
                    return Ok(Parsed::Exit(fatal(
                        "options '--name-only', '--name-status', '--check', and '-s' \
                         cannot be used together",
                    )));
                }
                match a {
                    "--name-only" => o.name_only = true,
                    "--name-status" => o.name_status = true,
                    _ => o.check = true,
                }
            }
            "--remerge-diff" | "--diff-merges=remerge" | "--diff-merges=r" => {
                o.remerge_diff = true
            }
            "--no-remerge-diff" => o.remerge_diff = false,
            // `diff_merges_parse_opts()` (diff-merges.c:117-150) and its five setup
            // functions. `--cc` is *not* here: format-patch's own option table claims
            // it for `Cc:` and `parse_options()` runs before `setup_revisions()`.
            "--no-diff-merges" => o.diff_merges = DiffMerges::Off,
            "-m" => o.diff_merges = diff_merges_default,
            "-c" => o.diff_merges = DiffMerges::Combined,
            "--dd" => o.diff_merges = DiffMerges::FirstParent,
            // `parse_long_opt()` (revision.c) also takes the separate form, and dies
            // `Option '<opt>' requires a value` when the next argv slot is missing.
            "--diff-merges" => {
                i += 1;
                let Some(v) = args.get(i).cloned() else {
                    return Ok(Parsed::Exit(fatal("Option '--diff-merges' requires a value")));
                };
                match parse_diff_merges(&v, diff_merges_default) {
                    Some(m) => o.diff_merges = m,
                    None if v == "remerge" || v == "r" => o.remerge_diff = true,
                    None => record_opt_error(
                        &mut o.opt_error,
                        i,
                        128,
                        format!("fatal: invalid value for '--diff-merges': '{v}'"),
                    ),
                }
            }
            s if s.starts_with("--diff-merges=") => {
                let value = &s["--diff-merges=".len()..];
                match parse_diff_merges(value, diff_merges_default) {
                    Some(m) => o.diff_merges = m,
                    // `set_diff_merges()`'s `die()`, so exit 128 and positional
                    // against the revisions like every other setup_revisions error.
                    None => record_opt_error(
                        &mut o.opt_error,
                        i,
                        128,
                        format!("fatal: invalid value for '--diff-merges': '{value}'"),
                    ),
                }
            }
            // `--color-moved[=<mode>]`, `--color-moved-ws=<modes>`,
            // `--word-diff[=<mode>]`, `--word-diff-regex=<re>` and
            // `--color-words[=<re>]`, shared with every other command built from
            // `add_diff_options()`. Two of them force colour on outright, which is
            // why `color_when` is threaded through the parser.
            s if diff_color::needs_separate_value(s) => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("error: {}", diff_color::missing_value(s));
                    return Ok(Parsed::Exit(ExitCode::from(129)));
                };
                let joined = format!("{s}={v}");
                if let Some(Err(msg)) = move_word.parse_flag(&joined, &mut color_when) {
                    record_opt_error(&mut o.opt_error, i, 129, msg);
                }
            }
            s if is_move_word_flag(s) => {
                if let Some(Err(msg)) = move_word.parse_flag(s, &mut color_when) {
                    record_opt_error(&mut o.opt_error, i, 129, msg);
                }
            }
            s if NO_OP.contains(&s) => {}
            // A bare value-taking short option eats the next slot before anything
            // else can read it as a revision; without that, `format-patch -S base`
            // reported `fatal: ambiguous argument 'base'` instead of naming the
            // option this module has not ported.
            s if DEFERRED_SHORT_VALUE.contains(&s) => {
                i += 1;
                if args.get(i).is_none() {
                    // `parse_short_opt()`'s `opterror(..., "requires a value")`,
                    // which `parse_options()` turns into exit 129 with that one
                    // line and no usage block. Measured against stock 2.55.0:
                    // `git format-patch --stdout -S` prints exactly this and
                    // exits 129.
                    let c = &s[1..];
                    eprintln!("error: switch `{c}' requires a value");
                    return Ok(Parsed::Exit(ExitCode::from(129)));
                }
                o.deferred.push(s.to_owned());
            }
            s if DEFERRED.iter().any(|f| is_flag(s, f))
                || DEFERRED_SHORT_VALUE.iter().any(|f| s.starts_with(f)) =>
            {
                o.deferred.push(s.to_owned());
            }
            // `--pretty=<fmt>` / `--format=<fmt>`. `cmd_format_patch()` starts from
            // `rev.commit_format = CMIT_FMT_EMAIL` (builtin/log.c:2107), so `email`
            // is what this module already renders and `mboxrd` is that plus
            // `pp_remainder()`'s `>` escape (pretty.c:2286). Every other format is a
            // different renderer this module does not have, so it stays refused
            // rather than silently producing email bytes under another name.
            s if matches!(
                s.split_once('=').map(|(n, v)| (n, v)),
                Some(("--pretty" | "--format", "email" | "mboxrd"))
            ) =>
            {
                o.pretty_mboxrd = s.ends_with("=mboxrd");
            }
            // The `SYMMETRIC_LEFT` family (revision.c:2483-2515). Each sets
            // `revs->limited`, so they only ever act on a walk that has both sides of
            // a symmetric range in it; on any other range every one of them is inert,
            // which is what `apply_cherry_limits` reproduces.
            "--cherry-pick" => {
                if o.cherry_mark {
                    return Ok(Parsed::Exit(fatal(
                        "options '--cherry-mark' and '--cherry-pick' cannot be used together",
                    )));
                }
                o.cherry_pick = true;
            }
            "--cherry-mark" => {
                if o.cherry_pick {
                    return Ok(Parsed::Exit(fatal(
                        "options '--cherry-mark' and '--cherry-pick' cannot be used together",
                    )));
                }
                o.cherry_mark = true;
            }
            "--left-only" => {
                if o.right_only {
                    return Ok(Parsed::Exit(fatal(
                        "options '--left-only' and '--right-only/--cherry' cannot be used together",
                    )));
                }
                o.left_only = true;
            }
            "--right-only" => {
                if o.left_only {
                    return Ok(Parsed::Exit(fatal(
                        "options '--right-only' and '--left-only' cannot be used together",
                    )));
                }
                o.right_only = true;
            }
            // `--cherry` is `--cherry-mark --right-only --max-parents=1`
            // (revision.c:2497-2504); format-patch already runs at `max_parents = 1`.
            "--cherry" => {
                if o.left_only {
                    return Ok(Parsed::Exit(fatal(
                        "options '--cherry' and '--left-only' cannot be used together",
                    )));
                }
                if o.cherry_pick {
                    return Ok(Parsed::Exit(fatal(
                        "options '--cherry-mark' and '--cherry-pick' cannot be used together",
                    )));
                }
                o.cherry_mark = true;
                o.right_only = true;
            }
            // `setup_revisions()` reports anything left over the same way, whether it
            // is a typo or an option git knows but this module does not reach.
            s if s.starts_with('-') => {
                return Ok(Parsed::Exit(fatal(&format!("unrecognized argument: {s}"))));
            }
            s => {
                // `add_pending_object_with_path()` clears `no_walk` the moment an
                // UNINTERESTING object joins the pending list, so an exclusion or
                // a range cancels it — and, being positional, a later `--no-walk`
                // turns it back on. A `--not` in force reverses which of them
                // count, exactly as it reverses the `^` prefix.
                if super::log::argument_excludes(s, negate) {
                    o.no_walk = false;
                }
                o.revs.push(RevWord::Rev {
                    spec: s.to_owned(),
                    pos: i,
                    negate,
                });
            }
        }
        i += 1;
    }

    // `--cover-from-description=<mode>` is validated only now, after the whole
    // command line has been parsed (git keeps it as a raw string and calls
    // `parse_cover_from_description()` once). An unrecognised mode is `die()`
    // (exit 128).
    if let Some(v) = cover_from_desc {
        match parse_cover_from_description(&v) {
            Some(m) => o.cover_from = m,
            None => {
                return Ok(Parsed::Exit(fatal(&format!(
                    "{v}: invalid cover from description mode"
                ))))
            }
        }
    }

    // builtin/log.c `cmd_format_patch()` `die()`s (exit 128) when `-k` is combined
    // with numbering or a subject prefix, since keep-subject suppresses both. The
    // numbering check comes first, so it wins when both conflicts are present.
    if o.keep_subject {
        if o.numbered == Some(true) {
            return Ok(Parsed::Exit(fatal(
                "options '-n' and '-k' cannot be used together",
            )));
        }
        if subject_prefix_given {
            return Ok(Parsed::Exit(fatal(
                "options '--subject-prefix/--rfc' and '-k' cannot be used together",
            )));
        }
    }

    // `--commit-list-format` on the command line implies `--cover-letter` when
    // the caller did not decide for themselves; `format.commitListFormat` does
    // not, because git only consults it after that check.
    if commit_list_format_given && !o.cover_letter_given {
        o.cover_letter = true;
        o.cover_letter_given = true;
    }

    // The paint layer, resolved once the whole command line has been read.
    // `git_diff_ui_config()` supplies the defaults each flag layers over, and
    // `--color-words`/`--word-diff=color` have already forced `color_when` to
    // `always`, so an explicit `--no-color` after one of them still wins.
    o.extra = match move_word.resolve(repo) {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("{msg}");
            return Ok(Parsed::Exit(ExitCode::from(128)));
        }
    };
    // The on/off decision is the flag and the terminal test alone. Measured against
    // stock 2.55.0: neither `color.diff`, `diff.color` nor `color.ui` moves it in
    // either direction — `-c color.diff=always format-patch --stdout` to a pipe stays
    // plain and `-c color.diff=never` to a pty still colours — while the very same
    // values do decide `git diff`. The `color.diff.<slot>` palette *is* read, so only
    // the switch is inert here, which is what `DiffColors::resolve` splits apart.
    let colors_on = match color_when {
        Some(ColorWhen::Always) => true,
        Some(ColorWhen::Never) => false,
        Some(ColorWhen::Auto) | None => {
            super::color::want_color_stdout_raw(repo, None)
        }
    };
    o.colors = DiffColors::resolve(repo, colors_on);
    o.ws_error_highlight = match ws_error_highlight {
        Some(v) => v,
        None => diff_color::ws_error_highlight_default(repo).unwrap_or(diff_color::WSEH_NEW),
    };

    o.branch_name = find_branch_name(repo, &o);

    // builtin/log.c: the stat+summary block is format-patch's default, but only
    // when the caller asked for no output format of its own — that is what makes
    // `--numstat` (and friends) *replace* it rather than add to it.
    if !o.use_patch_format && o.output_format == 0 {
        o.output_format = FMT_DIFFSTAT | FMT_SUMMARY;
    }

    Ok(Parsed::Ready(Box::new(o)))
}

/// `func_by_opt()` (diff-merges.c:68-85). `default_mode` is what `log.diffMerges`
/// left `set_to_default` at, which is the only thing `on`/`m` resolve through.
fn parse_diff_merges(value: &str, default_mode: DiffMerges) -> Option<DiffMerges> {
    match value {
        "off" | "none" => Some(DiffMerges::Off),
        "1" | "first-parent" => Some(DiffMerges::FirstParent),
        "separate" => Some(DiffMerges::Separate),
        "c" | "combined" => Some(DiffMerges::Combined),
        "cc" | "dense-combined" => Some(DiffMerges::DenseCombined),
        "m" | "on" => Some(default_mode),
        _ => None,
    }
}

/// Port of `git_parse_maybe_bool()` (config.c): the words git accepts as
/// booleans, case-insensitively. `None` means "this is not a boolean", which is
/// what makes `format.from`/`format.notes` take a value instead.
fn maybe_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => Some(true),
        "false" | "no" | "off" | "" => Some(false),
        s => s.parse::<i64>().ok().map(|n| n != 0),
    }
}

/// git's `git_committer_info(IDENT_NO_DATE)` reduced to name and address — the
/// identity `--from` and `format.from` fall back to.
fn committer_ident(repo: &gix::Repository) -> Result<Ident> {
    let sig = repo
        .committer()
        .transpose()?
        .ok_or_else(|| anyhow!("unable to auto-detect email address"))?;
    Ok(Ident {
        name: sig.name.to_str()?.to_owned(),
        mail: sig.email.to_str()?.to_owned(),
    })
}

/// Port of `parse_cover_from_description()` (builtin/log.c).
fn parse_cover_from_description(arg: &str) -> Option<CoverFrom> {
    match arg {
        "default" | "message" => Some(CoverFrom::Message),
        "none" => Some(CoverFrom::None),
        "subject" => Some(CoverFrom::Subject),
        "auto" => Some(CoverFrom::Auto),
        _ => None,
    }
}

/// Port of `base_callback()` (builtin/log.c).
fn set_base(o: &mut Opts, arg: &str) {
    if arg == "auto" {
        o.auto_base = AutoBase::Always;
        o.base_commit = None;
    } else {
        o.auto_base = AutoBase::Never;
        o.base_commit = Some(arg.to_owned());
    }
}

/// The branch whose `branch.<name>.description` the cover letter may quote.
///
/// Port of `cmd_format_patch()`'s `check_head` shortcut plus `find_branch_name()`
/// (builtin/log.c): a command line that walks HEAD implicitly — the bare
/// `<since>` shorthand, an explicit `HEAD`, or no revision at all — names the
/// current branch; otherwise the single *interesting* revision argument names a
/// branch iff it resolves to `refs/heads/<name>` at that exact tip.
fn find_branch_name(repo: &gix::Repository, o: &Opts) -> Option<String> {
    // `rev->cmdline` as `add_rev_cmdline()` fills it: the name each pending
    // object was named by, and whether it is UNINTERESTING. A range contributes
    // both of its sides, a ref-set selector one entry per ref it matched.
    let mut cmdline: Vec<(String, bool)> = Vec::new();
    for word in &o.revs {
        let (spec, negate) = match word {
            RevWord::Rev { spec, negate, .. } => (spec, *negate),
            RevWord::Refs(rs) => {
                cmdline.extend(ref_set_pending(repo, rs).into_iter().map(|(n, _)| (n, rs.negate)));
                continue;
            }
        };
        if let Some((l, r)) = spec.split_once("...") {
            // `handle_dotdot_1()` substitutes HEAD for an empty side before it
            // records either name.
            cmdline.push((or_head(l).to_owned(), negate));
            cmdline.push((or_head(r).to_owned(), negate));
        } else if let Some((l, r)) = spec.split_once("..") {
            cmdline.push((or_head(l).to_owned(), !negate));
            cmdline.push((or_head(r).to_owned(), negate));
        } else if let Some(rest) = spec.strip_prefix('^') {
            // `add_pending_object_with_path()` is handed `arg` *after* the `^`,
            // so `^HEAD` is still recorded under the name `HEAD`.
            cmdline.push((rest.to_owned(), !negate));
        } else {
            cmdline.push((spec.clone(), negate));
        }
    }
    if cmdline.is_empty() {
        // `revs->def` put HEAD on the pending list without an `add_rev_cmdline()`
        // of its own, and `cmd_format_patch` reads the *pending* entry's name.
        cmdline.push(("HEAD".to_owned(), false));
    }

    let head_branch = || {
        repo.head_ref()
            .ok()
            .flatten()
            .and_then(|r| r.name().as_bstr().to_str().ok().map(str::to_owned))
            .and_then(|n| n.strip_prefix("refs/heads/").map(str::to_owned))
            .or_else(|| Some(String::new()))
    };
    // `cmd_format_patch`'s `check_head`, which turns on `rev.pending.nr == 1`
    // alone — an excluded lone endpoint (`^<rev>`) counts, because the promotion
    // below adds HEAD next to it either way.
    if cmdline.len() == 1
        && ((o.max_count.is_none() && !o.root) || cmdline[0].0 == "HEAD")
    {
        return head_branch();
    }

    let mut interesting = cmdline.iter().filter(|(_, hidden)| !*hidden);
    let (name, _) = interesting.next()?;
    if interesting.next().is_some() {
        return None;
    }
    // `repo_dwim_ref()` hands back the ref name a symref *resolves to*, so a
    // plain `HEAD` on the command line still names the branch it points at.
    let mut r = repo.find_reference(name.as_str()).ok()?;
    while let Some(Ok(next)) = r.follow() {
        r = next;
    }
    let branch = r
        .name()
        .as_bstr()
        .to_str()
        .ok()?
        .strip_prefix("refs/heads/")?
        .to_owned();
    let tip = repo.rev_parse_single(BStr::new(name.as_str())).ok()?;
    let head = r.into_fully_peeled_id().ok()?;
    (head.detach() == tip.detach()).then_some(branch)
}

/// An empty side of a range means HEAD, as in `..main` or `main..`.
/// A named fn, not a closure: closure inference unifies the input and output
/// lifetimes into one variable that cannot outlive the call.
fn or_head(s: &str) -> &str {
    if s.is_empty() {
        "HEAD"
    } else {
        s
    }
}

/// The pending objects one `--all`-family selector contributes, as
/// `(name, commit id)` in `for_each_ref` order.
///
/// The name is the one `handle_one_ref()` is handed — trimmed of the namespace
/// prefix for `--branches`/`--tags`/`--remotes`, full for `--all`/`--glob` —
/// which is both what `--exclude` matches against and what `add_rev_cmdline()`
/// records for `find_branch_name()`. Refs that do not peel to a commit are
/// dropped: format-patch asks for neither trees nor blobs, so `handle_commit()`
/// has nothing to do with them.
fn ref_set_pending(repo: &gix::Repository, rs: &RefSet) -> Vec<(String, ObjectId)> {
    // `refs_for_each_ref_ext()` builds the pattern it matches full ref names
    // against, and appends the implied `/` `*` when there is no wildcard in it.
    let pattern = rs.pattern.as_ref().map(|p| {
        let mut pat = String::new();
        match rs.prefix {
            Some(prefix) => pat.push_str(prefix),
            None if !p.starts_with("refs/") => pat.push_str("refs/"),
            None => {}
        }
        pat.push_str(p);
        if !p.bytes().any(|b| matches!(b, b'?' | b'*' | b'[')) {
            if !pat.ends_with('/') {
                pat.push('/');
            }
            pat.push('*');
        }
        pat
    });
    let peel = |id: ObjectId| -> Option<ObjectId> {
        repo.find_object(id)
            .ok()
            .and_then(|o| o.peel_to_commit().ok())
            .map(|c| c.id)
    };

    let mut out: Vec<(String, ObjectId)> = Vec::new();
    let Ok(platform) = repo.references() else {
        return out;
    };
    let Ok(iter) = platform.all() else {
        return out;
    };
    for reference in iter.filter_map(Result::ok) {
        let full = reference.name().as_bstr().to_string();
        if let Some(prefix) = rs.prefix {
            if !full.starts_with(prefix) {
                continue;
            }
        }
        if let Some(pat) = &pattern {
            if !super::log::wildmatch(pat.as_bytes(), full.as_bytes()) {
                continue;
            }
        }
        let name = match rs.prefix {
            Some(prefix) => full[prefix.len()..].to_owned(),
            None => full,
        };
        if rs
            .excludes
            .iter()
            .any(|g| super::log::wildmatch(g.as_bytes(), name.as_bytes()))
        {
            continue;
        }
        let target = match reference.try_id() {
            Some(id) => id.detach(),
            None => match reference.into_fully_peeled_id() {
                Ok(id) => id.detach(),
                Err(_) => continue,
            },
        };
        if let Some(id) = peel(target) {
            out.push((name, id));
        }
    }
    // `--all` alone follows every ref with `refs_head_ref`, which is why it can
    // reach a detached HEAD that no ref points at.
    if rs.with_head && !rs.excludes.iter().any(|g| super::log::wildmatch(g.as_bytes(), b"HEAD")) {
        if let Some(id) = repo.head_id().ok().and_then(|h| peel(h.detach())) {
            out.push(("HEAD".to_owned(), id));
        }
    }
    out
}

/// Port of `parse_dirstat_params()` (diff.c), plus `parse_dirstat_opt()`'s
/// "and now DIRSTAT is one of the output formats" side effect.
///
/// git `die()`s with the accumulated message and exit 128; `bail!` would collapse
/// that to 1, so the message goes to stderr here and the code comes back as an
/// error the caller turns into an exit.
fn set_dirstat(o: &mut Opts, params: &str) -> std::result::Result<(), ExitCode> {
    let mut errmsg = String::new();
    if !params.is_empty() {
        for p in params.split(',') {
            match p {
                "changes" => {
                    o.dirstat.by_line = false;
                    o.dirstat.by_file = false;
                }
                "lines" => {
                    o.dirstat.by_line = true;
                    o.dirstat.by_file = false;
                }
                "files" => {
                    o.dirstat.by_line = false;
                    o.dirstat.by_file = true;
                }
                "noncumulative" => o.dirstat.cumulative = false,
                "cumulative" => o.dirstat.cumulative = true,
                _ if p.starts_with(|c: char| c.is_ascii_digit()) => match parse_permille(p) {
                    Some(permille) => o.dirstat.permille = permille,
                    None => errmsg.push_str(&format!(
                        "  Failed to parse dirstat cut-off percentage '{p}'\n"
                    )),
                },
                _ => errmsg.push_str(&format!("  Unknown dirstat parameter '{p}'\n")),
            }
        }
    }
    if !errmsg.is_empty() {
        return Err(fatal(&format!(
            "Failed to parse --dirstat/-X option parameter:\n{errmsg}"
        )));
    }
    o.output_format |= FMT_DIRSTAT;
    Ok(())
}

/// git's dirstat percentage grammar: whole percent, then at most one significant
/// fractional digit — `12.375` is 123 permille, and any trailing junk is fatal.
fn parse_permille(p: &str) -> Option<u32> {
    let digits = p.len() - p.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let (whole, rest) = p.split_at(digits);
    let mut permille = whole.parse::<u32>().ok()?.checked_mul(10)?;
    let rest = match rest.strip_prefix('.') {
        Some(frac) if frac.starts_with(|c: char| c.is_ascii_digit()) => {
            permille += u32::from(frac.as_bytes()[0] - b'0');
            frac.trim_start_matches(|c: char| c.is_ascii_digit())
        }
        _ => rest,
    };
    rest.is_empty().then_some(permille)
}

/// Port of `parse_opt_object_name()` (parse-options-cb.c): resolve `arg` to an
/// object name and append it. A name that resolves to nothing is parse-options'
/// own `error()`, which leaves git with exit 129 and no usage block.
fn push_interdiff(
    repo: &gix::Repository,
    o: &mut Opts,
    arg: &str,
) -> std::result::Result<(), ExitCode> {
    match repo.rev_parse_single(arg) {
        Ok(id) => {
            o.interdiff.push(id.detach());
            Ok(())
        }
        Err(_) => {
            eprintln!("error: malformed object name '{arg}'");
            Err(ExitCode::from(129))
        }
    }
}

/// Port of `diff_opt_ignore_regex()` (diff.c): `regcomp` failure is an
/// `error()`, which makes `parse_options` exit 129 with only that one line.
fn push_ignore_regex(o: &mut Opts, pattern: &str) -> std::result::Result<(), ExitCode> {
    match Regex::compile(pattern) {
        Ok(re) => {
            o.ignore_regex.push(re);
            Ok(())
        }
        Err(_) => {
            eprintln!("error: invalid regex given to -I: '{pattern}'");
            Err(ExitCode::from(129))
        }
    }
}

fn parse_num(s: &str) -> Result<usize> {
    s.parse::<usize>()
        .map_err(|_| anyhow!("invalid number `{s}`"))
}

/// `OPT_INTEGER(0, "filename-max-length", …)` — an `int`, so the value reads
/// base 0 with a `k`/`m`/`g` suffix and each rejection is one of
/// parse-options' three diagnostics at exit 129. git keeps a negative length as
/// it is and clamps at use, which here is the same as no limit.
fn parse_filename_max_length(value: &str) -> std::result::Result<usize, ExitCode> {
    match crate::optint::integer(&crate::optint::long_opt("filename-max-length"), value) {
        Ok(n) => Ok(n.max(0) as usize),
        Err(e) => {
            eprintln!("error: {e}");
            Err(ExitCode::from(129))
        }
    }
}

/// `diff_opt_find_renames()` / `diff_opt_find_copies()` (diff.c:5722-5756). Both
/// read `parse_rename_score()` off the front of the value and reject anything
/// left over with `error(_("invalid argument to %s"))`, which parse-options turns
/// into exit 129 with that one line and no usage block. The two differ only in
/// which detection they turn on, and in `--find-copies`' rule that a *second*
/// `-C` means `--find-copies-harder`.
fn set_rename_score(o: &mut Opts, idx: usize, name: &str, value: &str) {
    let (score, rest) = super::diffcore_rename::parse_rename_score(value);
    if !rest.is_empty() {
        record_opt_error(
            &mut o.opt_error,
            idx,
            129,
            format!("error: invalid argument to {name}"),
        );
        return;
    }
    o.rename_score = score;
    if name == "find-renames" {
        o.detect_rename = Some(super::diffcore_rename::DETECT_RENAME);
    } else if o.detect_rename == Some(super::diffcore_rename::DETECT_COPY) {
        o.find_copies_harder = true;
    } else {
        o.detect_rename = Some(super::diffcore_rename::DETECT_COPY);
    }
}

/// `diff_opt_char()`: the value must be exactly one byte wide. The error is
/// reported in place rather than recorded, because parse-options rejects it
/// before `setup_revisions()` reaches any revision — measured against stock,
/// `format-patch --output-indicator-new=ab HEAD~9` is the 129 here, not the
/// revision's 128.
fn set_indicator(
    o: &mut Opts,
    _idx: usize,
    name: &str,
    value: &str,
) -> std::result::Result<(), ExitCode> {
    if value.len() > 1 {
        eprintln!("error: {name} expects a character, got '{value}'");
        return Err(ExitCode::from(129));
    }
    let ch = value.as_bytes().first().copied().unwrap_or(0);
    match name {
        "output-indicator-new" => o.indicators.0 = ch,
        "output-indicator-old" => o.indicators.1 = ch,
        _ => o.indicators.2 = ch,
    }
    Ok(())
}

/// `diff_opt_break_rewrites()` (diff.c:5569-5590): `<n>[/<m>]` packed into one
/// int, with `error(_("%s expects <n>/<m> form"))` for anything else.
fn set_break_opt(o: &mut Opts, idx: usize, value: &str) {
    match super::diffcore_rename::parse_break_opt(value) {
        Ok(packed) => o.break_opt = packed,
        Err(()) => record_opt_error(
            &mut o.opt_error,
            idx,
            129,
            "error: break-rewrites expects <n>/<m> form".to_owned(),
        ),
    }
}

/// `-l<n>`, the `OPT_INTEGER` at diff.c:6188. parse-options reports a malformed
/// value as a *short* switch, `error: switch \`l' expects an integer value …`.
fn set_rename_limit(o: &mut Opts, idx: usize, value: &str) {
    match crate::optint::integer(&crate::optint::short_opt('l'), value) {
        Ok(n) => o.rename_limit = Some(n),
        Err(e) => record_opt_error(&mut o.opt_error, idx, 129, format!("error: {e}")),
    }
}

/// Record a diff-option value error the way git would report it from inside
/// `setup_revisions()`: it is not fatal in place, because a revision error at an
/// earlier command-line position preempts it. Only the earliest such error is
/// kept — parsing is left-to-right, so the first one recorded is the earliest.
fn record_opt_error(slot: &mut Option<(usize, u8, String)>, idx: usize, code: u8, msg: String) {
    if slot.is_none() {
        *slot = Some((idx, code, msg));
    }
}

/// git's `die(_("'%s': not an integer"))` for the revision-walk counts, recorded
/// positionally (exit 128).
/// `OPT_INTEGER`'s value handling for `--creation-factor`: an empty value and a
/// malformed one get parse-options' two different `error()` messages, both exit
/// 129 and both recorded so an earlier revision error still preempts them.
fn set_creation_factor(o: &mut Opts, idx: usize, value: &str) {
    if value.is_empty() {
        record_opt_error(
            &mut o.opt_error,
            idx,
            129,
            "error: option `creation-factor' expects a numerical value".to_owned(),
        );
        return;
    }
    match parse_int_with_suffix(value) {
        IntParse::Ok(n) => o.creation_factor = Some(n),
        IntParse::Bad => record_opt_error(
            &mut o.opt_error,
            idx,
            129,
            "error: option `creation-factor' expects an integer value with an optional \
             k/m/g suffix"
                .to_owned(),
        ),
        IntParse::Range => record_opt_error(
            &mut o.opt_error,
            idx,
            129,
            format!(
                "error: value {value} for option `creation-factor' not in range \
                 [-2147483648,2147483647]"
            ),
        ),
    }
}

fn not_an_integer(slot: &mut Option<(usize, u8, String)>, idx: usize, val: &str) {
    record_opt_error(slot, idx, 128, format!("fatal: '{val}': not an integer"));
}

/// Port of git's `strtol_i(s, 10, &result)`: skip leading ASCII whitespace, an
/// optional sign, then base-10 digits, and succeed only if the whole string is
/// consumed and at least one digit was seen (`p == s` and a trailing `*p` are
/// both failures). Overflow past `i64` is a failure too, matching git's
/// `(int)ul != ul` guard closely enough for the values git accepts.
fn strtol_i(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let neg = b.get(i) == Some(&b'-');
    if matches!(b.get(i), Some(b'+') | Some(b'-')) {
        i += 1;
    }
    let digit_start = i;
    let mut val: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        val = val.checked_mul(10)?.checked_add((b[i] - b'0') as i64)?;
        i += 1;
    }
    if i == digit_start || i != b.len() {
        return None;
    }
    Some(if neg { -val } else { val })
}

/// When a recorded diff-option value error is at an earlier command-line
/// position than a failing revision, git reports the option error instead.
/// Prints the stored message and returns its exit code, else `None`.
fn opt_preempts(o: &Opts, rev_pos: usize) -> Option<ExitCode> {
    match &o.opt_error {
        Some((p, code, msg)) if *p < rev_pos => {
            eprintln!("{msg}");
            Some(ExitCode::from(*code))
        }
        _ => None,
    }
}

/// git reaches a diff-option value error during `setup_revisions()` whenever no
/// earlier revision failed, so once every revision has resolved the recorded
/// error still fires. Prints the stored message and returns its exit code.
fn emit_opt_error(o: &Opts) -> Option<ExitCode> {
    o.opt_error.as_ref().map(|(_, code, msg)| {
        eprintln!("{msg}");
        ExitCode::from(*code)
    })
}

/// Port of the revision resolution `is_range_diff_range()` performs on
/// `--range-diff=<arg>`: each side of an `a..b`/`a...b` range (an empty side is
/// HEAD), or the bare argument, must resolve, else git `die()`s
/// `bad revision '<arg>'` (exit 128) after the walk. The range-diff render is not
/// ported, so a range git accepts is handled as an unsupported flag by the
/// caller; only the resolution failure is reproduced here.
fn validate_range_diff(repo: &gix::Repository, arg: &str) -> std::result::Result<(), ExitCode> {
    let ok_side = |side: &str| -> bool {
        let s = if side.is_empty() { "HEAD" } else { side };
        repo.rev_parse_single(BStr::new(s)).is_ok()
    };
    let ok = if let Some((l, r)) = arg.split_once("...") {
        ok_side(l) && ok_side(r)
    } else if let Some((l, r)) = arg.split_once("..") {
        ok_side(l) && ok_side(r)
    } else {
        ok_side(arg)
    };
    if ok {
        Ok(())
    } else {
        Err(fatal(&format!("bad revision '{arg}'")))
    }
}

/// Emulate C `strtoul(nptr, &end, 10)`, returning the accumulated base-10 value
/// and the byte offset of `end` — the first character the conversion did not
/// consume. Leading ASCII whitespace and a single optional sign are skipped;
/// when no digit is consumed there is "no conversion", so `end` is the original
/// pointer (offset 0) and the value 0, matching libc. The value saturates rather
/// than wrapping, which the callers' width arithmetic tolerates.
fn strtoul10(s: &[u8]) -> (i64, usize) {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let neg = s.get(i) == Some(&b'-');
    if matches!(s.get(i), Some(b'+') | Some(b'-')) {
        i += 1;
    }
    let digit_start = i;
    let mut val: i64 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        val = val.saturating_mul(10).saturating_add((s[i] - b'0') as i64);
        i += 1;
    }
    if i == digit_start {
        return (0, 0);
    }
    (if neg { -val } else { val }, i)
}

/// Port of the `--stat` branch of `diff_opt_stat()` (diff.c):
/// `width = strtoul(v); if (*end==',') name_width = strtoul(...); if
/// (*end==',') count = strtoul(...);` returning the parsed fields, or `None`
/// when anything is left over (`if (*end) return error(...)`). `width` is always
/// parsed (an empty value yields 0, git's "use the default" sentinel); the later
/// fields are `Some` only when their comma was reached, so an absent field keeps
/// git's "leave the previous value" behavior.
fn parse_stat_value(val: &[u8]) -> Option<(i64, Option<i64>, Option<i64>)> {
    let (width, mut off) = strtoul10(val);
    let mut name_width = None;
    let mut count = None;
    if val.get(off) == Some(&b',') {
        let (nw, e) = strtoul10(&val[off + 1..]);
        name_width = Some(nw);
        off = off + 1 + e;
    }
    if val.get(off) == Some(&b',') {
        let (c, e) = strtoul10(&val[off + 1..]);
        count = Some(c);
        off = off + 1 + e;
    }
    (off == val.len()).then_some((width, name_width, count))
}

/// Port of the scalar branches of `diff_opt_stat()` (diff.c) —
/// `--stat-width`/`--stat-name-width`/`--stat-graph-width`/`--stat-count`. Each
/// is `strtoul(value); if (*end) error(_("%s expects a numerical value"))`
/// (exit 129, recorded positionally like the other setup_revisions errors) and
/// each turns the diffstat on. `name` is git's dashless `opt->long_name`.
fn parse_stat_scalar(o: &mut Opts, name: &str, val: &str, idx: usize) {
    let (v, off) = strtoul10(val.as_bytes());
    if off != val.len() {
        record_opt_error(
            &mut o.opt_error,
            idx,
            129,
            format!("error: {name} expects a numerical value"),
        );
        return;
    }
    match name {
        "stat-width" => o.stat_width = v,
        "stat-name-width" => o.stat_name_width = v,
        "stat-graph-width" => o.stat_graph_width = v,
        "stat-count" => o.stat_count = v,
        _ => unreachable!("parse_stat_scalar called with an unknown option name"),
    }
    o.output_format |= FMT_DIFFSTAT;
}

/// The three outcomes of parsing an integer-with-suffix option value.
enum IntParse {
    /// A number in the signed 32-bit range git accepts for `--start-number`.
    Ok(i64),
    /// Not a number, or trailing junk / an unrecognised unit suffix.
    Bad,
    /// A well-formed number, but outside `[i32::MIN, i32::MAX]` after the unit.
    Range,
}

/// Port of C `strtoimax(s, &end, 0)`: optional leading ASCII whitespace, an
/// optional sign, then a base-0 numeral (`0x…` hex, `0…` octal, else decimal).
/// Returns the value and the number of bytes consumed, or `None` when no digit is
/// consumed. Accumulates in `i128` so an over-long numeral saturates rather than
/// wrapping; the caller's range check turns that into git's ERANGE.
fn strtoimax0(s: &[u8]) -> Option<(i128, usize)> {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let neg = match s.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let (base, start) = if s.get(i) == Some(&b'0') && matches!(s.get(i + 1), Some(b'x') | Some(b'X'))
    {
        (16i128, i + 2)
    } else if s.get(i) == Some(&b'0') {
        (8i128, i)
    } else {
        (10i128, i)
    };
    let mut j = start;
    let mut val: i128 = 0;
    // Saturate well past the i32 range the caller checks against, so an
    // arbitrarily long numeral becomes an out-of-range error rather than
    // wrapping, while every in-range value is left exact.
    let saturate = 1i128 << 40;
    while j < s.len() {
        let d = match s[j] {
            b'0'..=b'9' => (s[j] - b'0') as i128,
            b'a'..=b'f' => (s[j] - b'a' + 10) as i128,
            b'A'..=b'F' => (s[j] - b'A' + 10) as i128,
            _ => break,
        };
        if d >= base {
            break;
        }
        val = (val * base + d).min(saturate);
        j += 1;
    }
    if j == start {
        return None;
    }
    Some((if neg { -val } else { val }, j))
}

/// git parses `--start-number` as a signed integer with an optional `k`/`m`/`g`
/// unit (base-0, so hex and octal too) into a 4-byte int. This mirrors
/// parse-options.c's per-value handling: no digits after the sign is the "integer
/// value with an optional k/m/g suffix" error, an over-range magnitude is the
/// "not in range" error, and both are `error()` → exit 129.
fn parse_int_with_suffix(value: &str) -> IntParse {
    let b = value.as_bytes();
    let Some((mag, consumed)) = strtoimax0(b) else {
        return IntParse::Bad;
    };
    let factor: i128 = match &b[consumed..] {
        [] => 1,
        [c] if c.eq_ignore_ascii_case(&b'k') => 1024,
        [c] if c.eq_ignore_ascii_case(&b'm') => 1024 * 1024,
        [c] if c.eq_ignore_ascii_case(&b'g') => 1024 * 1024 * 1024,
        _ => return IntParse::Bad,
    };
    let total = mag * factor;
    if total < i32::MIN as i128 || total > i32::MAX as i128 {
        return IntParse::Range;
    }
    IntParse::Ok(total as i64)
}

/// Validate a `--start-number` value the way git's parse-options does, returning
/// the number to use or the exit code git would print for. builtin/log.c clamps a
/// negative start number to 1 after parsing, so that is folded in here.
fn parse_start_number(value: &str) -> std::result::Result<usize, ExitCode> {
    if value.is_empty() {
        eprintln!("error: option `start-number' expects a numerical value");
        return Err(ExitCode::from(129));
    }
    match parse_int_with_suffix(value) {
        IntParse::Ok(v) => Ok(if v < 0 { 1 } else { v as usize }),
        IntParse::Bad => {
            eprintln!(
                "error: option `start-number' expects an integer value with an \
                 optional k/m/g suffix"
            );
            Err(ExitCode::from(129))
        }
        IntParse::Range => {
            eprintln!(
                "error: value {value} for option `start-number' not in range \
                 [-2147483648,2147483647]"
            );
            Err(ExitCode::from(129))
        }
    }
}

/// The value slot of a two-token option, e.g. the `<dir>` in `-o <dir>`.
///
/// Every option this is called for is an entry in `builtin_format_patch_options`
/// (builtin/log.c:2006-2095), so a missing value is `get_arg()`'s
/// `PARSE_OPT_ERROR` (parse-options.c:59-60) and not this port's own diagnostic:
/// one `error: <optname> requires a value` line on stderr, **no usage block**,
/// exit 129. `cmd_format_patch` runs that `parse_options()` sweep to completion
/// before `setup_revisions()` looks at a single argument (builtin/log.c:2196),
/// which is why the refusal is immediate here and outranks a bad revision
/// written earlier on the command line.
///
/// `tok` is the option as the user spelled it, so [`crate::parseopt::OptName::typed`] gives
/// `optname()`'s two renderings from the one call site: `git format-patch -o`
/// is ``switch `o'`` and `git format-patch --output-directory` is
/// ``option `output-directory'``.
fn value_at(args: &[String], i: usize, tok: &str) -> Result<String> {
    crate::parseopt::value_at(args, i, crate::parseopt::OptName::typed(tok))
        .map(str::to_string)
}

/// Port of `clean_message_id()` (builtin/log.c): skip leading whitespace and
/// `<`, then take through the last byte that is neither whitespace nor `>`. The
/// caller wraps the result back in `<`/`>`. `None` is git's
/// `die("insane in-reply-to: …")` (exit 128) when no such byte exists.
fn clean_message_id(msg_id: &str) -> Option<String> {
    let b = msg_id.as_bytes();
    let is_space = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r');
    let mut a = 0;
    while a < b.len() && (is_space(b[a]) || b[a] == b'<') {
        a += 1;
    }
    let mut z: Option<usize> = None;
    let mut m = a;
    while m < b.len() {
        if !is_space(b[m]) && b[m] != b'>' {
            z = Some(m);
        }
        m += 1;
    }
    z.map(|z| String::from_utf8_lossy(&b[a..=z]).into_owned())
}

/// Read a `--signature-file`, whose contents become the trailing signature
/// verbatim. git `die()`s (exit 128) `unable to read signature file '<f>': <err>`
/// when it cannot be read; the common missing-file / permission errnos are
/// reproduced from `ErrorKind`.
fn read_signature_file(path: &str) -> std::result::Result<String, ExitCode> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Err(e) => {
            let reason = match e.kind() {
                std::io::ErrorKind::NotFound => "No such file or directory".to_owned(),
                std::io::ErrorKind::PermissionDenied => "Permission denied".to_owned(),
                _ => e
                    .raw_os_error()
                    .map(|n| format!("os error {n}"))
                    .unwrap_or_else(|| e.to_string()),
            };
            Err(fatal(&format!(
                "unable to read signature file '{path}': {reason}"
            )))
        }
    }
}

/// Port of the signature-resolution ladder in `cmd_format_patch` (builtin/log.c),
/// run once revisions are resolved. git keeps four inputs — the `signature`
/// pointer (`--signature`/`--no-signature`, else the version default),
/// `signature_file_arg` (`--signature-file`), and the
/// `format.signature`/`format.signatureFile` config — and resolves them in this
/// order:
///   * `--no-signature` inhibits every signature;
///   * an explicit `--signature` is used verbatim (an empty value renders none);
///   * else a `--signature-file`, or a `format.signatureFile` *only when no
///     `format.signature` is set*, is read from disk — an unreadable file is
///     `die_errno` → exit 128, with the `--signature-file` argument preferred
///     over the config when both are present;
///   * else `format.signature`;
///   * else the version default.
fn resolve_signature(o: &Opts) -> std::result::Result<String, ExitCode> {
    match &o.sig_cli {
        SigCli::No => Ok(String::new()),
        SigCli::Value(s) => Ok(s.clone()),
        SigCli::Unset => {
            if o.sig_file_arg.is_some()
                || (o.cfg_signature_file.is_some() && o.cfg_signature.is_none())
            {
                let path = o
                    .sig_file_arg
                    .as_deref()
                    .or(o.cfg_signature_file.as_deref())
                    .expect("a file path is present by the condition above");
                read_signature_file(path)
            } else if let Some(s) = &o.cfg_signature {
                Ok(s.clone())
            } else {
                Ok(SIGNATURE_VERSION.to_owned())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Revision selection
// ---------------------------------------------------------------------------

/// `--diff-merges=<mode>` and the `-m`/`-c`/`--cc`/`--dd` spellings, as
/// `diff-merges.c`'s five setup functions leave `rev_info`. `remerge` is refused
/// separately (builtin/log.c:2220-2227), so it has no variant here.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DiffMerges {
    /// `set_none()`: `--diff-merges=off|none`, `--no-diff-merges`, and
    /// format-patch's own default — `cmd_format_patch` sets neither
    /// `separate_merges` nor `combine_merges`.
    Off,
    /// `set_separate()`: `--diff-merges=separate|m|on` and `-m`. One diff, and one
    /// repeat of the mail header block, per parent.
    Separate,
    /// `set_first_parent()`: `--diff-merges=first-parent|1` and `--dd`.
    FirstParent,
    /// `set_combined()`: `--diff-merges=combined|c` and `-c`.
    Combined,
    /// `set_dense_combined()`: `--diff-merges=dense-combined|cc` and `--cc`.
    DenseCombined,
}

/// What `--ignore-if-in-upstream` finds on the pending list once
/// `cmd_format_patch`'s single-endpoint rule has run.
enum Endpoints {
    /// Two endpoints naming the same object: git's `goto done`, a silent exit 0.
    Identical,
    /// Two distinct endpoints — a range `get_patch_ids()` accepts.
    Range,
    /// Any other count: `get_patch_ids()`'s `need exactly one range`.
    NotOne,
    /// Two endpoints that are both interesting or both uninteresting, which
    /// `get_patch_ids()` refuses with `not a range`.
    NotARange,
}

/// Classify the pending list the way `cmd_format_patch` leaves it for
/// `--ignore-if-in-upstream`, in the order it asks the questions: the two
/// endpoints being one object is the silent `goto done`, then `get_patch_ids()`
/// wants exactly two of them, then it wants their UNINTERESTING flags to differ.
///
/// The single-endpoint promotion the count is taken after is skipped when
/// `--max-count`/`-<n>` or `--root` was given, which is exactly when
/// `format-patch --root HEAD --ignore-if-in-upstream` still dies with one endpoint.
fn upstream_endpoints(repo: &gix::Repository, opts: &Opts) -> Result<Endpoints> {
    // A revision `setup_revisions()` would have died on: it reports itself a
    // moment later, from `select_commits`, so say nothing about it here.
    let Ok(p) = seed_pending(repo, opts)? else {
        return Ok(Endpoints::Range);
    };
    match (p.tips.as_slice(), p.hidden.as_slice()) {
        ([a], [b]) | ([a, b], []) | ([], [a, b]) if a == b => Ok(Endpoints::Identical),
        ([_], [_]) => Ok(Endpoints::Range),
        ([_, _], []) | ([], [_, _]) => Ok(Endpoints::NotARange),
        _ => Ok(Endpoints::NotOne),
    }
}

fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// Port of `output_directory_callback()` (builtin/log.c:1593-1603).
///
/// `-o`/`--output-directory` is `OPT_CALLBACK_F(…, PARSE_OPT_NONEG,
/// output_directory_callback)` (builtin/log.c:2041-2043) rather than an
/// `OPT_STRING`, and the callback refuses to overwrite a value it already holds:
///
/// ```text
/// const char **dir = (const char **)opt->value;
/// BUG_ON_OPT_NEG(unset);
/// if (*dir)
///         die(_("two output directories?"));
/// *dir = arg;
/// ```
///
/// So the last `-o` does **not** win the way a repeated `--signature` or
/// `--subject-prefix` does — a second one is fatal (exit 128), in every spelling
/// the option has (`-o <dir>`, `-o<dir>`, `--output-directory <dir>`,
/// `--output-directory=<dir>`) and in any mixture of them, since all four reach
/// the one callback. `*dir` is a pointer test, so even `-o '' -o x` is two
/// directories. `PARSE_OPT_NONEG` means there is no `--no-output-directory` that
/// could clear it back to `NULL`.
///
/// The `die()` fires inside `parse_options()`, which walks the whole argv before
/// `setup_revisions()` runs, so it preempts an unresolvable revision later on
/// the line — which is why this is checked here in the option loop rather than
/// recorded positionally like the `diff_opt_parse()` value errors are.
///
/// `format.outputDirectory` is a different variable
/// (`cfg.config_output_directory`, builtin/log.c:895) merged in only at
/// builtin/log.c:2261-2262, i.e. after this check, so config plus one `-o` is
/// not "two output directories" — hence the flag rather than a test on
/// [`Opts::outdir`], which carries the config seed.
fn set_outdir(o: &mut Opts, dir: String) -> Result<(), ExitCode> {
    if o.outdir_cli {
        return Err(fatal("two output directories?"));
    }
    o.outdir = Some(dir);
    o.outdir_cli = true;
    Ok(())
}

/// The author timestamp `--author-date-order` sorts on, which is git's
/// `record_author_date()`: the raw epoch seconds off the `author` header, with
/// the zone offset ignored. A commit whose header cannot be read keeps the
/// slab's zero, so it sorts oldest.
fn author_date(repo: &gix::Repository, id: ObjectId) -> i64 {
    let Ok(object) = repo.find_object(id) else {
        return 0;
    };
    if object.kind != gix::object::Kind::Commit {
        return 0;
    }
    object
        .into_commit()
        .author()
        .map(|a| a.seconds())
        .unwrap_or(0)
}

/// What `handle_revision_arg_1()` made of a single-name operand — the
/// `get_oid_with_context()` + `get_reference()` pair it runs once the range
/// spellings have had their turn.
enum Named {
    /// The object `get_reference()` appended to `revs->pending`, **as named**.
    ///
    /// Of whatever type: `handle_revision_arg_1()` type-checks nothing, so a
    /// tree, a blob or a tag lands on the pending list exactly like a commit
    /// and is dropped later, by `handle_commit()` — see
    /// [`crate::objname::walk_pending`]. Peeling to a commit here instead is
    /// what used to turn `format-patch <tree>` into an `ambiguous argument`
    /// failure, because a tree that would not peel looked like a name that had
    /// not resolved.
    Pending(ObjectId),
    /// `verify_non_filename()`'s `die()`, carried rather than raised because an
    /// earlier diff-option error may still preempt it.
    ///
    /// It sits between the two calls above, and before `get_reference()`:
    ///
    /// ```c
    /// if (get_oid_with_context(revs->repo, arg, get_sha1_flags, &oid, &oc))
    ///         return revs->ignore_missing ? 0 : -1;
    /// if (!cant_be_filename)
    ///         verify_non_filename(revs->prefix, arg);
    /// object = get_reference(revs, arg, &oid, flags ^ local_flags);
    /// ```
    ///
    /// so a name that resolves *and* names a working-tree file is refused ahead
    /// of `bad object`, and a `--` anywhere on the line
    /// (`REVARG_CANNOT_BE_FILENAME`) turns the check off entirely — which is what
    /// `git format-patch -- <name>` is for. The rule itself is
    /// [`crate::setup::verify_non_filename`]'s, shared with every verb that reads
    /// a revision out of argv; its first line is the `is_inside_work_tree()`
    /// guard that keeps a bare repository from calling every operand ambiguous.
    BothRevisionAndFilename(String),
    /// `get_reference()`'s `die("bad object %s")`: the name decoded — which for
    /// a full-length hex happens without the object database being consulted at
    /// all, see [`crate::objname`] — but `parse_object()` found nothing.
    BadObject,
    /// `get_oid_with_context()` failed outright, so `handle_revision_arg()`
    /// returns non-zero and the word gets its filename reading.
    Unresolved,
}

enum Selected {
    Commits {
        commits: Vec<ObjectId>,
        /// Pathspecs, including positionals that turned out to name a path.
        paths: Vec<String>,
        /// `rev.pending` as `prepare_revision_walk()` found it: every object the
        /// revision arguments resolved to, interesting or not. Only `--no-walk`
        /// needs it, because that is the one path where a commit can reach the
        /// output while still unparsed — see [`render_cover_letter`].
        pending: Vec<ObjectId>,
    },
    Exit(ExitCode),
}

/// `rev.pending` split the way the walk consumes it.
struct Pending {
    /// The interesting endpoints, in command-line order — which is the order the
    /// commit-date queue breaks ties in, so it is load-bearing.
    tips: Vec<ObjectId>,
    /// The left endpoint of every `<a>...<b>` on the command line, i.e. the objects
    /// `handle_dotdot_1()` pends with `SYMMETRIC_LEFT` (revision.c:2107). Only the
    /// `--cherry-pick`/`--left-only`/`--right-only` family reads it; the flag itself
    /// propagates to ancestors, which [`symmetric_left_side`] reproduces.
    symmetric_left: Vec<ObjectId>,
    /// The UNINTERESTING endpoints, plus the merge bases a `<a>...<b>` excludes.
    hidden: Vec<ObjectId>,
    /// Positionals that named a path rather than a revision.
    paths: Vec<String>,
    /// `revs->rev_input_given`: set by every revision argument that resolved and
    /// by every `--all`-family selector, whether or not that selector matched
    /// anything. It is what keeps `s_r_opt.def` from adding HEAD behind the
    /// caller's back — `format-patch --tags` in a repository with no tags formats
    /// nothing rather than falling back to HEAD.
    rev_input_given: bool,
}

/// `setup_revisions()`'s left-to-right pass over the revision words: resolve each
/// one and put what it names on the pending list.
///
/// The `Err` arm is git's `die()` for a word that names neither a revision nor a
/// path, ordered against a recorded diff-option value error by command-line
/// position the way git's single pass orders them.
fn seed_pending(
    repo: &gix::Repository,
    o: &Opts,
) -> Result<std::result::Result<Pending, ExitCode>> {
    // `repo_get_oid()` first, then `parse_object()` — git's order, and the
    // ambiguity warning a full-length hex that is also a ref name earns. Both
    // live in [`crate::objname`] because every command that takes an object name
    // from argv needs them; resolving through `rev_parse_single()` alone reports
    // "not a valid object name" for a well-formed id the repository simply does
    // not have, and never warns.
    let resolve = |spec: &str| -> Named {
        let Some(id) = crate::objname::resolve(repo, spec) else {
            return Named::Unresolved;
        };
        // `verify_non_filename()` before `get_reference()` — see
        // [`Named::BothRevisionAndFilename`] for the C.
        if !o.seen_dashdash {
            if let Some(message) = crate::setup::verify_non_filename(repo, spec) {
                return Named::BothRevisionAndFilename(message);
            }
        }
        match repo.find_object(id) {
            Ok(_) => Named::Pending(id),
            Err(_) => Named::BadObject,
        }
    };

    let mut p = Pending {
        tips: Vec::new(),
        symmetric_left: Vec::new(),
        hidden: Vec::new(),
        paths: o.paths.clone(),
        rev_input_given: false,
    };

    for word in &o.revs {
        // A ref-set selector cannot fail: `handle_one_ref()` simply adds what it
        // finds, in `for_each_ref` order, and adds nothing when it finds nothing.
        let (spec, negate, rpos) = match word {
            RevWord::Rev { spec, negate, pos } => (spec, *negate, *pos),
            RevWord::Refs(rs) => {
                p.rev_input_given = true;
                let side = if rs.negate { &mut p.hidden } else { &mut p.tips };
                side.extend(ref_set_pending(repo, rs).into_iter().map(|(_, id)| id));
                continue;
            }
        };
        // git resolves revisions and diff options in one left-to-right pass, so
        // a recorded diff-option value error preempts this revision iff it sits
        // earlier on the command line. `rev_err` defers computing the revision
        // error (which prints its own message) until that check has passed.
        let rev_err = |compute: &dyn Fn() -> ExitCode| -> ExitCode {
            match opt_preempts(o, rpos) {
                Some(e) => e,
                None => compute(),
            }
        };
        // `handle_dotdot()` is the first thing `handle_revision_arg_1()` tries,
        // and `handle_dotdot_1()` is the *whole* of the range rule: endpoint
        // resolution by `get_oid_with_context()`, `parse_object()` on both, and
        // — for `<a>...<b>` only — `lookup_commit_reference()` on each end. Ask
        // [`crate::objname`] rather than re-deriving it here; the copy that used
        // to stand in this spot peeled every endpoint straight to a commit,
        // which collapsed git's three separate endings into one and printed
        // `Invalid revision range` even for a symmetric difference.
        //
        // A `NotARange` answer is `handle_dotdot()` returning non-zero, which in
        // git is not an error at all — control simply falls through to the `^`
        // and single-name branches below, so `^<a>..<b>` still ends as
        // `bad revision '<token>'` and `nosuchthing..HEAD` as the ambiguous
        // argument.
        //
        // A bare `..` never gets that far: it is the pathspec for the parent
        // directory (see [`crate::objname::is_parent_directory_pathspec`]), so
        // it falls through to the single-name branch, fails to resolve, and
        // joins the prune data.
        let range = (!crate::objname::is_parent_directory_pathspec(spec, o.seen_dashdash))
            .then(|| crate::objname::split_range(spec))
            .flatten()
            .map(|r| {
                // `handle_dotdot_1()` resolves both endpoints through
                // `get_oid_with_context()`, so this is where an endpoint that is
                // a full-length hex *and* a ref name earns its warning.
                // [`crate::objname::dotdot`] is quiet by design — it is asked
                // twice per operand here, once to classify and once to
                // diagnose — so the warning is requested separately, exactly
                // once, alongside the resolution the walk keeps.
                crate::objname::warn_dotdot_endpoints(repo, spec);
                (r.symmetric, crate::objname::dotdot(repo, spec))
            });
        if let Some((symmetric, crate::objname::Dotdot::Missing { .. })) = &range {
            // `dotdot_missing()`, with whatever `lookup_commit_reference()`
            // already printed ahead of it. Rendered here but reported through
            // `rev_err`, which may find an earlier diff-option error preempts it.
            let message = crate::objname::dotdot_fatal(&repo, spec)
                .unwrap_or_else(|| format!("fatal: {}\n", crate::objname::dotdot_missing_message(spec, *symmetric)));
            return Ok(Err(rev_err(&|| {
                eprint!("{message}");
                ExitCode::from(128)
            })));
        }
        // `handle_revision_arg_1()`'s three-mark block, which sits between the
        // range rule above and the single-name rule below. It is quoted in full
        // on [`crate::objname::parents_only`]; what it decides here is only
        // *which name is resolved*, because `get_oid_1()` has no case for `^@`,
        // `^!` or `^-<n>` and an operand that still carries one cannot resolve
        // at all. Skipping the block is therefore not a naming detail:
        // `format-patch --stdout HEAD^!` formatted nothing before v0.16.0 and
        // was `ambiguous argument` after it, against stock's one patch.
        //
        // A marked operand is never also a range — `<a>..<b>^!`'s right endpoint
        // does not resolve, so `handle_dotdot()` has already declined it — which
        // is why testing the marks after the range branch is still git's order.
        let spec: &str = match crate::objname::parents_only(spec) {
            // No mark at all, and — for `^-0`, `^--1` and the like — a parent
            // number `handle_revision_arg_1()` refused before
            // `add_parents_only()` was reached. Both leave the operand exactly as
            // typed; the refused number then fails to resolve, which is the
            // `ambiguous argument` git's own `ret = -1` ends at.
            crate::objname::ParentsOnly::Absent | crate::objname::ParentsOnly::BadParent => spec,
            crate::objname::ParentsOnly::Mark { base, nth, replaces } => {
                // `^@` queues the parents under `flags`; `^!` and `^-<n>` under
                // `flags ^ (UNINTERESTING | BOTTOM)`, so under a preceding
                // `--not` all three flip with it.
                let sense = if replaces { negate } else { !negate };
                let mut queue = |_name: &str, parent, uninteresting: bool| {
                    if uninteresting { p.hidden.push(parent) } else { p.tips.push(parent) }
                };
                match crate::objname::add_parents_only(repo, base, sense, nth, &mut queue) {
                    // `get_reference()`'s `die(_("bad object %s"), name)`, raised
                    // from inside `add_parents_only()`'s tag-peeling loop and
                    // naming the base rather than the operand:
                    // `format-patch <absent-40-hex>^!` is
                    // `fatal: bad object <absent-40-hex>` in stock 2.55.0.
                    crate::objname::Parents::BadObject => {
                        let name = crate::objname::uninteresting_mark(base).0;
                        return Ok(Err(rev_err(&|| fatal(&format!("bad object {name}")))));
                    }
                    // `add_parents_only()` answered 0 — a name that did not
                    // resolve, a non-commit, or a `^-<n>` past the parent count —
                    // so `arg` is left alone and the operand carries its mark
                    // into the resolution below, which cannot succeed.
                    crate::objname::Parents::None => spec,
                    // `if (add_parents_only(…)) { ret = 0; goto out; }`: the
                    // operand itself is never queued.
                    crate::objname::Parents::Queued if replaces => {
                        p.rev_input_given = true;
                        continue;
                    }
                    // `arg = arg_minus_excl`: the parents are in, and the
                    // truncated name goes on to the single-name path — which is
                    // what makes `<rev>^!` the range `<rev>^..<rev>`.
                    crate::objname::Parents::Queued => base,
                }
            }
        };
        if let Some((symmetric, crate::objname::Dotdot::Ok { a, b })) = range {
            p.rev_input_given = true;
            // The endpoints go on the pending list as the objects git pended —
            // for `<a>..<b>` of whatever type, since only the symmetric form
            // type-checks. A tree or a blob is dropped later, by
            // `prepare_revision_walk()`, and *not* here: `cmd_format_patch`'s
            // `<since>` shorthand counts `rev.pending.nr` while the entry is
            // still on the list, so dropping it early would turn
            // `<tree>..HEAD` into a one-object pending list and format nothing.
            if symmetric {
                // `a...b` is everything reachable from either tip but not both.
                // Both sides carry the sense in force and the merge bases carry
                // its opposite — `handle_dotdot_1()`'s `flags_exclude`.
                let (sides, bases) = if negate {
                    (&mut p.hidden, &mut p.tips)
                } else {
                    (&mut p.tips, &mut p.hidden)
                };
                sides.push(a);
                sides.push(b);
                // `a_flags = flags | SYMMETRIC_LEFT` (revision.c:2107). Under
                // `--not` the two ends swap sense but not sides: git ORs
                // `SYMMETRIC_LEFT` onto whichever object `a` names either way.
                p.symmetric_left.push(a);
                // Both ends are commits by now: the symmetric form ran them
                // through `lookup_commit_reference()` before it got here.
                for base in repo.merge_bases_many(a, &[b])? {
                    bases.push(base.detach());
                }
            } else if negate {
                // The left side always carries the opposite sense of the right.
                p.tips.push(a);
                p.hidden.push(b);
            } else {
                p.hidden.push(a);
                p.tips.push(b);
            }
        } else if let Some(rest) = spec.strip_prefix('^') {
            match resolve(rest) {
                Named::Pending(id) => {
                    p.rev_input_given = true;
                    // `^` flips the sense `--not` set, as git XORs both.
                    if negate {
                        p.tips.push(id);
                    } else {
                        p.hidden.push(id);
                    }
                }
                Named::BothRevisionAndFilename(message) => {
                    return Ok(Err(rev_err(&|| fatal(&message))))
                }
                // `get_reference()` names the argument it was handed, which is
                // the operand *after* the `^` was stripped.
                Named::BadObject => {
                    return Ok(Err(rev_err(&|| fatal(&format!("bad object {rest}")))))
                }
                // An exclusion is never retried as a filename.
                Named::Unresolved => {
                    return Ok(Err(rev_err(&|| {
                        fatal(&format!("bad revision '{spec}'"))
                    })))
                }
            }
        } else {
            match resolve(spec) {
                Named::Pending(id) => {
                    p.rev_input_given = true;
                    if negate {
                        p.hidden.push(id);
                    } else {
                        p.tips.push(id);
                    }
                }
                Named::BothRevisionAndFilename(message) => {
                    return Ok(Err(rev_err(&|| fatal(&message))))
                }
                Named::BadObject => {
                    return Ok(Err(rev_err(&|| fatal(&format!("bad object {spec}")))))
                }
                // `handle_revision_arg()` returned non-zero. With a `--` anywhere
                // on the line `REVARG_CANNOT_BE_FILENAME` is in force and the word
                // has nowhere left to go.
                Named::Unresolved if o.seen_dashdash => {
                    return Ok(Err(rev_err(&|| {
                        fatal(&format!("bad revision '{spec}'"))
                    })))
                }
                // Otherwise this word and *every word after it* are prune data, so
                // git verifies them all as filenames and then stops resolving
                // revisions entirely. It leaves `rev_input_given` alone, which is
                // what lets `s_r_opt.def` still supply HEAD.
                Named::Unresolved => {
                    let rest = revision_argv_from(&o.argv, rpos);
                    if let Some(msg) = verify_filenames(repo, &rest) {
                        return Ok(Err(rev_err(&|| fatal(&msg))));
                    }
                    p.paths.extend(rest.iter().map(|s| (*s).to_owned()));
                    break;
                }
            }
        }
    }

    // Every revision resolved (or became a pathspec). git still reaches any
    // recorded diff-option value error during the same pass, so it fires now.
    if let Some(e) = emit_opt_error(o) {
        return Ok(Err(e));
    }

    // `setup_revisions()`'s last act: `s_r_opt.def` ("HEAD") joins a pending list
    // that no revision argument contributed to. An unborn HEAD is
    // `diagnose_missing_default()`, which dies rather than formatting nothing.
    if p.tips.is_empty() && p.hidden.is_empty() && !p.rev_input_given {
        let head = repo.head()?;
        if head.is_unborn() {
            let branch = head
                .referent_name()
                .map(|n| n.shorten().to_str_lossy().into_owned())
                .unwrap_or_else(|| "master".to_owned());
            return Ok(Err(fatal(&format!(
                "your current branch '{branch}' does not have any commits yet"
            ))));
        }
        p.tips.push(repo.head_id()?.detach());
    }

    // `cmd_format_patch`: "This is traditional behaviour of `git format-patch
    // origin` that prepares what the origin side still does not have." With
    // exactly one pending object it is marked UNINTERESTING and HEAD is appended,
    // turning `<since>` into `<since>..HEAD`; `-<n>`/`--max-count` and `--root`
    // opt out and leave `get_revision()` the usual traversal.
    //
    // git counts *pending objects*, not positive arguments. `^<rev>` is one of
    // them and gets the same treatment — re-marking an already-UNINTERESTING
    // object changes nothing, so `format-patch ^<rev>` walks `<rev>..HEAD` just
    // like `format-patch <rev>` — while `^<a> <b>` is two and gets none.
    //
    // `add_head_to_pending()` gives up quietly when HEAD does not resolve, so on
    // an unborn branch the endpoint is left excluded and nothing is formatted.
    if p.tips.len() + p.hidden.len() == 1 && o.max_count.is_none() && !o.root {
        if let Some(since) = p.tips.pop() {
            p.hidden.push(since);
        }
        if let Ok(head) = repo.head_id() {
            p.tips.push(head.detach());
        }
    }

    Ok(Ok(p))
}

/// `SYMMETRIC_LEFT`'s reach: the walked commits that descend from a `<a>...<b>`
/// left endpoint.
///
/// git sets the flag on the pending object (revision.c:2107) and
/// `add_parents_to_list()` passes it down to every parent it queues
/// (revision.c:1179), so the left side is exactly "reachable from `a`". The walk
/// already stopped at the merge bases, so a breadth-first pass over the parent map
/// the walk recorded reproduces that reach without reading a single extra object —
/// and a `<a>...<b>` whose `a` is an ancestor of `b` correctly yields nothing,
/// because `a` never entered the list.
fn symmetric_left_side(
    left_tips: &[ObjectId],
    parents_of: &HashMap<ObjectId, Vec<ObjectId>>,
    walked: &[ObjectId],
) -> HashSet<ObjectId> {
    let in_list: HashSet<ObjectId> = walked.iter().copied().collect();
    let mut left: HashSet<ObjectId> = HashSet::new();
    let mut queue: Vec<ObjectId> = left_tips
        .iter()
        .copied()
        .filter(|id| in_list.contains(id))
        .collect();
    for id in &queue {
        left.insert(*id);
    }
    while let Some(id) = queue.pop() {
        for parent in parents_of.get(&id).into_iter().flatten() {
            if in_list.contains(parent) && left.insert(*parent) {
                queue.push(*parent);
            }
        }
    }
    left
}

/// `cherry_pick_list()` (revision.c:1217) followed by `limit_left_right()`
/// (revision.c:1421), in that order.
///
/// `cherry_pick_list()` counts both sides, returns immediately unless both are
/// non-empty, computes patch ids for the *smaller* side and then marks every
/// equal-patch-id commit on either side. `format-patch` renders neither
/// `PATCHSAME` nor a left/right mark, so `--cherry-mark` alone drops nothing —
/// measured against stock 2.55.0, `--cherry-mark --right-only` and `--right-only`
/// produce identical bytes.
fn apply_cherry_limits(
    repo: &gix::Repository,
    o: &Opts,
    symmetric_left: &[ObjectId],
    parents_of: &HashMap<ObjectId, Vec<ObjectId>>,
    walked: &mut Vec<ObjectId>,
) -> Result<()> {
    let left = symmetric_left_side(symmetric_left, parents_of, walked);
    // `if (!left_count || !right_count) return;` — with one side empty there is
    // nothing to compare against and nothing to keep.
    let left_count = walked.iter().filter(|id| left.contains(*id)).count();
    let right_count = walked.len() - left_count;
    if left_count == 0 || right_count == 0 {
        return Ok(());
    }

    let mut shown: HashSet<ObjectId> = HashSet::new();
    if o.cherry_pick {
        // `left_first = left_count < right_count`: patch ids are computed for
        // whichever side is smaller, and the other side is what gets searched.
        let left_first = left_count < right_count;
        let mut ids: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
        for id in walked.iter() {
            if left_first != left.contains(id) {
                continue;
            }
            let commit = repo.find_object(*id)?.try_into_commit()?;
            ids.entry(commit_patch_id(repo, &commit)?).or_default().push(*id);
        }
        for id in walked.iter() {
            if left_first == left.contains(id) {
                continue;
            }
            let commit = repo.find_object(*id)?.try_into_commit()?;
            let Some(same) = ids.get(&commit_patch_id(repo, &commit)?) else {
                continue;
            };
            // `commit->object.flags |= cherry_flag` for the commit found, and the
            // same for every commit its patch id was recorded from.
            shown.insert(*id);
            shown.extend(same.iter().copied());
        }
    }
    if o.right_only {
        shown.extend(walked.iter().filter(|id| left.contains(*id)).copied());
    } else if o.left_only {
        shown.extend(walked.iter().filter(|id| !left.contains(*id)).copied());
    }
    walked.retain(|id| !shown.contains(id));
    Ok(())
}

/// Resolve the revision arguments into the commits to format, oldest first and
/// with merges dropped (git sets `rev.max_parents = 1`).
///
/// A lone endpoint with neither `-<n>` nor `--root` is git's traditional
/// `format-patch <since>` shorthand for `<since>..HEAD`; anything else is an
/// ordinary walk over the given tips and exclusions.
fn select_commits(repo: &gix::Repository, o: &Opts) -> Result<Selected> {
    let Pending {
        tips,
        hidden,
        paths,
        symmetric_left,
        ..
    } = match seed_pending(repo, o)? {
        Ok(p) => p,
        Err(code) => return Ok(Selected::Exit(code)),
    };
    // `prepare_revision_walk()` runs `handle_commit()` over the pending list, and
    // that is where a tree or a blob endpoint disappears — silently, because
    // `rev.tree_objects`/`rev.blob_objects` are off for format-patch. It happens
    // *after* `cmd_format_patch` has counted `rev.pending.nr`, which is why the
    // filter is here rather than in `seed_pending`.
    let walkable = |ids: Vec<ObjectId>| -> Vec<ObjectId> {
        ids.into_iter().filter_map(|id| crate::objname::walk_pending(repo, id)).collect()
    };
    let (tips, hidden) = (walkable(tips), walkable(hidden));

    // `parse_pathspec()` runs inside `setup_revisions()`, so a malformed spec is
    // fatal before a single commit is walked — including on the paths below that
    // never consult the set.
    let specs = match paths.is_empty() {
        true => None,
        false => Some(super::log::PathspecMatcher::new(repo, &paths)?),
    };

    if tips.is_empty() {
        return Ok(Selected::Commits {
            commits: Vec::new(),
            paths,
            pending: Vec::new(),
        });
    }

    // `rev.pending` as the walk is about to consume it — `handle_commit()` parses
    // every one of these, which is what makes their trees readable later even
    // under `--no-walk`.
    let pending: Vec<ObjectId> = tips.iter().chain(hidden.iter()).copied().collect();

    // The walk, plus each commit's real parents — `sort_in_topological_order()`
    // needs them and so does the merge filter below.
    let mut walked: Vec<ObjectId> = Vec::new();
    let mut parents_of: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    if o.no_walk {
        // `prepare_revision_walk()` returns before `limit_list()` under
        // `--no-walk`, so the list is exactly the pending commits — sorted by
        // commit date unless `--no-walk=unsorted` kept the command-line order.
        // The UNINTERESTING ones are still on it, but `get_commit_action()`
        // ignores every one of them, which is what dropping them here does.
        let excluded: HashSet<ObjectId> = hidden.iter().copied().collect();
        let mut seen = HashSet::new();
        walked = tips
            .iter()
            .copied()
            .filter(|id| !excluded.contains(id) && seen.insert(*id))
            .collect();
        if !o.unsorted_input {
            let dates: HashMap<ObjectId, i64> = walked
                .iter()
                .map(|id| (*id, super::rev_list::commit_date(repo, *id)))
                .collect();
            walked.sort_by_key(|id| std::cmp::Reverse(dates[id]));
        }
        for id in &walked {
            parents_of.insert(*id, super::rev_list::commit_parents(repo, *id));
        }
    } else {
        let mut platform = repo
            .rev_walk(tips.clone())
            .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
        if o.first_parent {
            platform = platform.first_parent_only();
        }
        if !hidden.is_empty() {
            platform = platform.with_hidden(hidden);
        }
        for info in platform.all()? {
            let info = info?;
            parents_of.insert(info.id, info.parent_ids.to_vec());
            walked.push(info.id);
        }
    }

    // `limit_list()` simplifies the history over the pathspec before anything is
    // ordered or counted. `prepare_revision_walk()` returns before that call
    // under `--no-walk`, so a pathspec limits nothing there.
    if let Some(specs) = specs.as_ref() {
        if !o.no_walk {
            prune_treesame(repo, &mut walked, &parents_of, &tips, specs, o.first_parent)?;
        }
    }

    // `cherry_pick_list()` (revision.c:1489) and `limit_left_right()`
    // (revision.c:1492) are the last two steps of `limit_list()`, so they run over
    // the walked list *before* it is ordered and before the parent bounds, `--skip`
    // and `-<n>` cut it down. `--no-walk` returns from `prepare_revision_walk()`
    // ahead of `limit_list()` entirely, which is why none of them apply there.
    if !o.no_walk && (o.cherry_pick || o.left_only || o.right_only) {
        apply_cherry_limits(repo, o, &symmetric_left, &parents_of, &mut walked)?;
    }

    // `sort_in_topological_order()` runs inside `prepare_revision_walk()`, over
    // the whole list and before anything is emitted, so an ordering flag
    // reshuffles the walk and only then do the parent-count bounds, `--skip` and
    // `-<n>` cut it down. `--no-walk` returns from `prepare_revision_walk()`
    // *before* that call, so it silently disables the ordering flags: what
    // survives is the plain commit-date sort of the pending list above.
    if o.order != Order::Default && !o.no_walk {
        let dates: Option<HashMap<ObjectId, i64>> = match o.order {
            // `REV_SORT_IN_GRAPH_ORDER` has no date tie-break at all.
            Order::Topo | Order::Default => None,
            Order::DateTopo => Some(
                walked
                    .iter()
                    .map(|id| (*id, super::rev_list::commit_date(repo, *id)))
                    .collect(),
            ),
            Order::AuthorDateTopo => Some(
                walked
                    .iter()
                    .map(|id| (*id, author_date(repo, *id)))
                    .collect(),
            ),
        };
        walked = super::rev_list::topo_sort(&walked, &parents_of, dates.as_ref());
    }

    let mut out: Vec<ObjectId> = Vec::new();
    let mut skipped = 0usize;
    for id in walked {
        let parents = parents_of.get(&id).map_or(0, Vec::len);
        if parents < o.min_parents {
            continue;
        }
        if o.max_parents.is_some_and(|max| parents > max) {
            continue;
        }
        if skipped < o.skip {
            skipped += 1;
            continue;
        }
        if o.max_count.is_some_and(|max| out.len() >= max) {
            break;
        }
        out.push(id);
    }
    // The walk is newest-first; git emits oldest-first unless asked to reverse.
    if !o.reverse {
        out.reverse();
    }
    Ok(Selected::Commits {
        commits: out,
        paths,
        pending,
    })
}

/// Port of `try_to_simplify_commit()` + `get_commit_action()`'s TREESAME arm
/// (revision.c), which is what `-- <pathspec>` does to the walk.
///
/// A commit whose tree matches a parent's over the pathspec is TREESAME: it is
/// not shown, and the history it stands on is pruned to that one parent, so the
/// other branches of a merge stop being walked at all. Whatever the pruned
/// ancestry no longer reaches from the tips is therefore never seen — that is
/// the reachability sweep below, not an optimisation.
///
/// format-patch sets neither `rewrite_parents` nor `children`, so
/// `want_ancestry()` is false and a TREESAME commit is dropped whatever its
/// parent count. A root commit is compared against the empty tree, so it shows
/// exactly when it introduced a matching path.
///
/// Observed against stock git 2.55.0 over a fixture whose only `README.md`
/// change is the root commit: `format-patch -1 -- README.md` formats that root
/// commit and nothing else, while `format-patch -1 -- nosuch` formats nothing
/// and exits 0.
fn prune_treesame(
    repo: &gix::Repository,
    walked: &mut Vec<ObjectId>,
    parents_of: &HashMap<ObjectId, Vec<ObjectId>>,
    tips: &[ObjectId],
    specs: &super::log::PathspecMatcher,
    first_parent: bool,
) -> Result<()> {
    // id → (the parents the simplified history follows, whether it is shown).
    let mut simplified: HashMap<ObjectId, (Vec<ObjectId>, bool)> =
        HashMap::with_capacity(walked.len());
    for id in walked.iter() {
        let all = parents_of.get(id).map_or(&[][..], Vec::as_slice);
        // `--first-parent` limits the comparison the same way it limits the walk.
        let parents = if first_parent { &all[..all.len().min(1)] } else { all };
        if parents.is_empty() {
            let shown = touches_pathspec(repo, *id, None, specs)?;
            simplified.insert(*id, (Vec::new(), shown));
            continue;
        }
        let mut treesame: Option<ObjectId> = None;
        for p in parents {
            if !touches_pathspec(repo, *id, Some(*p), specs)? {
                treesame = Some(*p);
                break;
            }
        }
        match treesame {
            Some(p) => simplified.insert(*id, (vec![p], false)),
            None => simplified.insert(*id, (parents.to_vec(), true)),
        };
    }

    let mut reachable: HashSet<ObjectId> = HashSet::with_capacity(walked.len());
    let mut stack: Vec<ObjectId> = tips.to_vec();
    while let Some(id) = stack.pop() {
        if !reachable.insert(id) {
            continue;
        }
        if let Some((parents, _)) = simplified.get(&id) {
            stack.extend(parents.iter().copied());
        }
    }
    walked.retain(|id| {
        reachable.contains(id) && simplified.get(id).is_some_and(|(_, shown)| *shown)
    });
    Ok(())
}

/// `rev_compare_tree()`: does anything the pathspec selects differ between this
/// commit and `parent` (or the empty tree)?
///
/// `diff_tree_oid()` applies the pathspec while it walks and rename detection
/// never runs here, so a rename into or out of the set counts as a change on
/// whichever side the set contains.
fn touches_pathspec(
    repo: &gix::Repository,
    commit: ObjectId,
    parent: Option<ObjectId>,
    specs: &super::log::PathspecMatcher,
) -> Result<bool> {
    let new_tree = repo.find_object(commit)?.try_into_commit()?.tree()?;
    let old_tree = match parent {
        Some(p) => Some(repo.find_object(p)?.try_into_commit()?.tree()?),
        None => None,
    };
    let changes =
        repo.diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), gix::diff::Options::default())?;
    Ok(changes
        .iter()
        .filter(|c| !is_tree_entry(c))
        .any(|c| specs.matches(change_path(c))))
}

// ---------------------------------------------------------------------------
// Message rendering
// ---------------------------------------------------------------------------

/// `[v<n>-]NNNN-<sanitized subject><suffix>`, or the bare number under
/// `--numbered-files`. Port of `fmt_output_subject()` (log-tree.c).
fn patch_filename(commit: &gix::Commit<'_>, nr: usize, opts: &Opts) -> Result<String> {
    if opts.numbered_files {
        return Ok(nr.to_string());
    }
    let msg = skip_blank_lines(commit.message_raw()?);
    // git's `%f` sanitizes only the first line of the subject.
    let first_line = &msg[..one_line(msg)];
    Ok(numbered_filename(nr, trim_end_ws(first_line), opts))
}

/// The cover letter is always patch zero, whatever `--start-number` moved the
/// rest of the series to.
fn cover_filename(opts: &Opts) -> String {
    if opts.numbered_files {
        return "0".to_owned();
    }
    numbered_filename(0, b"cover letter", opts)
}

fn numbered_filename(nr: usize, subject: &[u8], opts: &Opts) -> String {
    let mut name = String::new();
    if let Some(r) = &opts.reroll {
        sanitize_subject(&mut name, format!("v{r}").as_bytes());
        name.push('-');
    }
    name.push_str(&format!("{nr:04}-"));
    sanitize_subject(&mut name, subject);

    let max = opts.name_max - (opts.suffix.len() + 1);
    if name.len() > max {
        // `sanitize_subject` only emits ASCII, so this is a char boundary.
        name.truncate(max);
    }
    name.push_str(&opts.suffix);
    name
}

/// Port of `format_sanitized_subject()` (pretty.c): collapse everything that is
/// not `[A-Za-z0-9._]` into single dashes, fold runs of dots, and trim trailing
/// `.`/`-`.
fn sanitize_subject(out: &mut String, msg: &[u8]) {
    let start_len = out.len();
    let mut space = 2u8;
    let mut i = 0;
    while i < msg.len() {
        let c = msg[i];
        if c.is_ascii_alphanumeric() || c == b'.' || c == b'_' {
            if space == 1 {
                out.push('-');
            }
            space = 0;
            out.push(c as char);
            if c == b'.' {
                while i + 1 < msg.len() && msg[i + 1] == b'.' {
                    i += 1;
                }
            }
        } else {
            space |= 1;
        }
        i += 1;
    }
    while out.len() > start_len && (out.ends_with('.') || out.ends_with('-')) {
        out.pop();
    }
}

/// Render one complete mail message: magic `From` line, headers, body, and —
/// when the commit changes anything — the three-dash separator, stat/summary
/// block and patch, followed by the signature.
#[allow(clippy::too_many_arguments)]
fn render_message(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    nr: usize,
    total: usize,
    opts: &Opts,
    th: &ThreadState,
    notes_trees: &[super::notes::Tree],
    out: &mut Vec<u8>,
) -> Result<()> {
    let msg_start = out.len();
    let mut raw_spans: Vec<(usize, usize)> = Vec::new();
    render_message_body(repo, commit, nr, total, opts, th, notes_trees, out, &mut raw_spans)?;
    // Everything a patch message is made of takes `--line-prefix`: `show_log()`
    // opens with `graph_show_commit()`, `log_write_email_headers()` follows each
    // header with `graph_show_oneline()`, `graph_show_strbuf()` writes it between
    // every pair of message lines, and `log_tree_diff_flush()` (log-tree.c:952) and
    // every `emit_diff_symbol()` write `diff_line_prefix()` themselves. What is left
    // outside is written by `cmd_format_patch` directly — the `base-commit:` trailer
    // and the signature — and `show_diff_of_diff()`, which installs an
    // `output_prefix` callback of its own (diff-lib.c:717) that replaces this one.
    if let Some(prefix) = &opts.line_prefix {
        // Prefix every stretch between the spans that go out raw, back to front so
        // the offsets ahead of each edit stay valid.
        let mut cut = out.len();
        for &(start, end) in raw_spans.iter().rev() {
            prefix_lines(out, end, cut, prefix.as_bytes());
            cut = start;
        }
        prefix_lines(out, msg_start, cut, prefix.as_bytes());
    }
    Ok(())
}

/// Insert `prefix` at the start of every line of `out[from..to]`.
fn prefix_lines(out: &mut Vec<u8>, from: usize, to: usize, prefix: &[u8]) {
    if prefix.is_empty() || from >= to || to > out.len() {
        return;
    }
    let mut rebuilt = Vec::with_capacity(to - from + prefix.len() * 8);
    for line in out[from..to].split_inclusive(|&b| b == b'\n') {
        rebuilt.extend_from_slice(prefix);
        rebuilt.extend_from_slice(line);
    }
    out.splice(from..to, rebuilt);
}

fn render_message_body(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    nr: usize,
    total: usize,
    opts: &Opts,
    th: &ThreadState,
    notes_trees: &[super::notes::Tree],
    out: &mut Vec<u8>,
    raw_spans: &mut Vec<(usize, usize)>,
) -> Result<()> {
    let header_start = out.len();
    write_from_line(out, commit.id, opts)?;
    // No prefix argument here: `show_log()` prefixes every line of a patch message,
    // which [`render_message`] applies to the whole region in one pass.
    write_message_ids(out, th, "")?;

    // Headers and body are built in one buffer because git's wrapping and the
    // final `strbuf_rtrim` both depend on what is already in it.
    let mut sb = String::new();
    let raw = commit.message_raw()?;

    let author = commit.author()?;
    let author = Ident {
        name: author
            .name
            .to_str()
            .map_err(|_| {
                anyhow!("author name is not valid UTF-8; RFC2047 encoding needs a known charset")
            })?
            .to_owned(),
        mail: author
            .email
            .to_str()
            .map_err(|_| {
                anyhow!("author email is not valid UTF-8; RFC2047 encoding needs a known charset")
            })?
            .to_owned(),
    };
    let date = commit
        .author()?
        .time()?
        .format(gix::date::time::format::GIT_RFC2822)?;

    // `--from`: the header names the given identity and the commit's own author
    // moves into an in-body `From:`, unless the two already agree (git's
    // `use_in_body_from()`, which `--force-in-body-from` short-circuits).
    let (header_ident, in_body_from) = match &opts.from {
        Some(from) if opts.force_in_body_from || !ident_eq(from, &author) => (
            from,
            Some(format!("From: {} <{}>\n", author.name, author.mail)),
        ),
        Some(from) => (from, None),
        None => (&author, None),
    };
    write_identity_headers(
        &mut sb,
        &header_ident.name,
        &header_ident.mail,
        &date,
        opts.encode_email_headers,
    );

    // Subject: — the first paragraph, folded onto one logical line, unless
    // `-k`/`--keep-subject` asked for the raw first paragraph (newlines and all).
    let msg = skip_blank_lines(raw);
    let (joined, rest) = format_subject(msg);
    let title = if opts.keep_subject {
        let consumed = &msg[..msg.len() - rest.len()];
        trim_end_ws(consumed)
            .to_str()
            .map_err(|_| anyhow!("commit subject is not valid UTF-8"))?
            .to_owned()
    } else {
        joined
            .to_str()
            .map_err(|_| anyhow!("commit subject is not valid UTF-8"))?
            .to_owned()
    };
    write_subject(&mut sb, &title, nr, total, opts);

    // git's `need_8bit_cte`: `-1` (never) under `--attach`/`--inline`, since the
    // multipart block declares the encoding itself; otherwise the committer
    // identity decides it when `--signoff` will append their trailer, and
    // failing that any non-ASCII byte in the message or the in-body headers.
    let signoff_needs_8bit = opts.signoff && {
        let c = committer_ident(repo)?;
        non_ascii(&c.name) || non_ascii(&c.mail)
    };
    let need_8bit = opts.mime_boundary.is_none()
        && (signoff_needs_8bit
            || raw.iter().any(|&b| b >= 0x80)
            || in_body_from.as_deref().is_some_and(non_ascii));
    if need_8bit {
        sb.push_str("MIME-Version: 1.0\n");
        sb.push_str(&format!("Content-Type: text/plain; charset={ENCODING}\n"));
        sb.push_str("Content-Transfer-Encoding: 8bit\n");
    }
    // `--add-header`, then `To:`/`Cc:`, follow the identity/MIME headers, and
    // the multipart preamble follows those (git builds one `extra_headers`
    // strbuf in that order).
    write_extra_headers(&mut sb, opts);
    write_mime_preamble(&mut sb, opts);
    sb.push('\n');
    // The in-body `From:` sits at the very top of the body, set off by a blank
    // line, and is not part of what `--signoff` treats as the message.
    if let Some(h) = &in_body_from {
        sb.push_str(h);
        sb.push('\n');
    }

    // Body — the remaining paragraphs, right-trimmed line by line.
    let beginning_of_body = sb.len();
    let mut body: Vec<u8> = Vec::new();
    pp_remainder_tabs(rest, &mut body, opts.expand_tabs);
    mboxrd_escape(&mut body, opts);
    sb.push_str(
        body.to_str()
            .map_err(|_| anyhow!("commit message is not valid UTF-8"))?,
    );
    while sb.ends_with([' ', '\t', '\n', '\r']) {
        sb.pop();
    }
    sb.push('\n');
    if sb.len() <= beginning_of_body {
        sb.push('\n');
    }
    // `rev.add_signoff` runs `append_signoff()` over the whole pretty-printed
    // message — headers included, since that is the buffer git hands it — with
    // `APPEND_SIGNOFF_DEDUP`, so a trailer block that already carries this
    // `Signed-off-by:` anywhere is left alone.
    if opts.signoff {
        let c = committer_ident(repo)?;
        super::commit::append_signoff(&mut sb, &format!("{} <{}>", c.name, c.mail), 0, true);
    }

    // Notes open their own commentary block, which is what makes the `---` line
    // before the diffstat collapse to a bare blank line.
    let mut shown_dashes = false;
    let notes = super::notes::format_display(repo, notes_trees, commit.id, false)?;
    if !notes.is_empty() {
        sb.push_str("---\n");
        shown_dashes = true;
        sb.push_str(
            notes
                .to_str()
                .map_err(|_| anyhow!("note is not valid UTF-8"))?,
        );
    }
    out.extend_from_slice(sb.as_bytes());

    // The mail headers and the message are one block; `log_tree_diff()` re-runs
    // `show_log()` for every parent it diffs against, so the separate-merge form
    // repeats these exact bytes ahead of each parent's patch.
    let header_block = out[header_start..].to_vec();

    // `log_tree_diff()` (log-tree.c:1097) decides what a merge gets. With
    // `--diff-merges=off` — format-patch's default, since it sets neither
    // `separate_merges` nor `combine_merges` — a merge falls through to
    // `log_tree_commit()`'s `always_show_header` branch and carries no three-dash
    // separator, no diffstat and no patch, whatever diff format was asked for.
    let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
    if parents.len() > 1 {
        match opts.diff_merges {
            DiffMerges::Off => return Ok(()),
            // `first_parent_merges` breaks out of the parent loop after the first.
            DiffMerges::FirstParent => {
                emit_commit_diff(repo, out, commit, Some(parents[0]), nr, opts, shown_dashes, raw_spans)?;
            }
            // The loop at log-tree.c:1156-1172, which sets `opt->loginfo` again for
            // each later parent so `show_log()` writes the whole header block anew.
            //
            // `log_tree_diff_flush()` returns *before* `show_log()` when the pair
            // queue came out empty (log-tree.c:929-936), so a parent this commit
            // does not differ from — or whose pairs `--diff-filter` dropped — gets no
            // message at all, and the next parent that does becomes the first record.
            DiffMerges::Separate => {
                let mut shown = false;
                for parent in &parents {
                    let mut body: Vec<u8> = Vec::new();
                    let mut spans: Vec<(usize, usize)> = Vec::new();
                    emit_commit_diff(
                        repo, &mut body, commit, Some(*parent), nr, opts, shown_dashes, &mut spans,
                    )?;
                    if body.is_empty() {
                        continue;
                    }
                    if shown {
                        // `show_log()`'s `opt->shown_one` newline (log-tree.c:770-790),
                        // which separates every record after the first.
                        out.push(b'\n');
                        out.extend_from_slice(&header_block);
                    }
                    let base = out.len();
                    out.extend_from_slice(&body);
                    raw_spans.extend(spans.iter().map(|(a, b)| (a + base, b + base)));
                    shown = true;
                }
                // Nothing shown at all leaves the header block already written, which
                // is `log_tree_commit()`'s `always_show_header` fallback.
            }
            DiffMerges::Combined | DiffMerges::DenseCombined => {
                emit_combined_diff(repo, out, commit, &parents, nr, opts, raw_spans)?;
            }
        }
        return Ok(());
    }
    emit_commit_diff(repo, out, commit, parents.first().copied(), nr, opts, shown_dashes, raw_spans)
}

/// One commit-versus-parent patch: the stat blocks and the patch body, in the order
/// `log_tree_diff_flush()` writes them.
fn emit_commit_diff(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    parent: Option<ObjectId>,
    nr: usize,
    opts: &Opts,
    shown_dashes: bool,
    raw_spans: &mut Vec<(usize, usize)>,
) -> Result<()> {
    let new_tree = commit.tree()?;
    let old_tree = match parent {
        Some(pid) => Some(repo.find_object(pid)?.try_into_commit()?.tree()?),
        None => None,
    };
    let abbrev = index_abbrev(repo, &new_tree, opts)?;
    let mut dissimilarity = HashMap::new();
    let mut changes = tree_changes(
        repo,
        old_tree.as_ref(),
        Some(&new_tree),
        opts.pathspec.as_ref(),
        opts.active_relative(),
        &RenameOpts::from_opts(opts),
        &mut dissimilarity,
    )?;
    rotate_changes(&mut changes, opts);
    apply_diff_filter(&mut changes, opts, &dissimilarity);

    if !changes.is_empty() {
        let mut patch: Vec<u8> = Vec::new();
        let r = render_changes(repo, &mut patch, &changes, abbrev, opts, &dissimilarity)?;

        let stat_sep = mime_stat_sep(commit, nr, opts)?;
        emit_stat_blocks(
            repo,
            out,
            &r.kept,
            &r.stats,
            opts,
            shown_dashes,
            stat_sep.as_deref(),
            &dissimilarity,
            raw_abbrev(repo, &new_tree, opts)?,
            opts.output_format,
            true,
            raw_spans,
        )?;
        out.extend_from_slice(&patch);
    }
    Ok(())
}

/// `-c` / `--cc`: `do_diff_combined()` (log-tree.c:975) into
/// `diff_tree_combined_merge()`.
///
/// Two things separate this from the per-parent form. `show_combined_diff()` writes
/// its own blank line after the header rather than the three-dash separator, and
/// `find_paths_generic()` computes every *stat* format against the first parent
/// alone (combine-diff.c:1368-1400) — only the patch body is the combined one.
fn emit_combined_diff(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    parents: &[ObjectId],
    nr: usize,
    opts: &Opts,
    raw_spans: &mut Vec<(usize, usize)>,
) -> Result<()> {
    // The bare newline `diff_tree_combined()` prints after `show_log()` when any
    // output format is on (combine-diff.c:1510-1515), in place of the three-dash
    // separator — which is what passing `shown_dashes = true` reproduces. Only the
    // `STAT_FORMAT_MASK` formats run in that pass; `--raw` is answered by
    // `show_raw_diff()` over the combined path set instead.
    let stat_mask = opts.output_format & (FMT_DIFFSTAT | FMT_NUMSTAT | FMT_SHORTSTAT | FMT_DIRSTAT | FMT_SUMMARY);
    emit_commit_diff_stats_only(repo, out, commit, parents[0], nr, opts, stat_mask, raw_spans)?;

    let dense = opts.diff_merges == DiffMerges::DenseCombined;
    let abbrev = crate::abbrev::configured_abbrev(repo, repo.object_hash().len_in_hex());
    // `if (num_paths)` guards both the raw block and `needsep` (combine-diff.c:1606).
    // A path only enters the intersection by differing from every parent, so it
    // always renders either hunks or a mode line: the patch being non-empty is the
    // same test as `num_paths`.
    let patch = super::diff::merge_combined_patch_painted(
        repo,
        commit.id,
        parents,
        &opts.paths,
        opts.context,
        dense,
        &opts.colors,
    )?;
    if patch.is_empty() {
        return Ok(());
    }
    // `--raw` under a combined mode is `show_raw_diff()`'s `::`-prefixed block, not
    // the first parent's raw lines, and it sets `needsep` on its own.
    if opts.output_format & FMT_RAW != 0 {
        out.extend_from_slice(&super::diff::merge_combined_raw(
            repo,
            commit.id,
            parents,
            &opts.paths,
            abbrev,
            false,
            true,
        )?);
    }
    if opts.output_format & (FMT_RAW | stat_mask) != 0 {
        out.push(b'\n');
    }
    out.extend_from_slice(&patch);
    Ok(())
}

/// The stat half of [`emit_commit_diff`] with the three-dash separator already
/// spent, for the combined form whose patch body comes from elsewhere.
fn emit_commit_diff_stats_only(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    commit: &gix::Commit<'_>,
    parent: ObjectId,
    nr: usize,
    opts: &Opts,
    output_format: u32,
    raw_spans: &mut Vec<(usize, usize)>,
) -> Result<()> {
    let new_tree = commit.tree()?;
    let old_tree = repo.find_object(parent)?.try_into_commit()?.tree()?;
    let abbrev = index_abbrev(repo, &new_tree, opts)?;
    let mut dissimilarity = HashMap::new();
    let mut changes = tree_changes(
        repo,
        Some(&old_tree),
        Some(&new_tree),
        opts.pathspec.as_ref(),
        opts.active_relative(),
        &RenameOpts::from_opts(opts),
        &mut dissimilarity,
    )?;
    rotate_changes(&mut changes, opts);
    apply_diff_filter(&mut changes, opts, &dissimilarity);
    if changes.is_empty() {
        // The blank line stands in for the three-dash separator whether or not the
        // stat pass has anything to say, so it is written here too.
        out.push(b'\n');
        return Ok(());
    }
    let mut discard: Vec<u8> = Vec::new();
    let r = render_changes(repo, &mut discard, &changes, abbrev, opts, &dissimilarity)?;
    let stat_sep = mime_stat_sep(commit, nr, opts)?;
    emit_stat_blocks(
        repo,
        out,
        &r.kept,
        &r.stats,
        opts,
        true,
        stat_sep.as_deref(),
        &dissimilarity,
        raw_abbrev(repo, &new_tree, opts)?,
        output_format,
        false,
        raw_spans,
    )
}

/// `diff_flush_patch_all_file_pairs()`: the commit's whole patch is decomposed into
/// `o->emitted_symbols` and re-emitted in one pass, which is what lets
/// `--color-moved` recognise a block that moved between two files of the same
/// commit and what `--word-diff` rewrites each hunk through.
fn paint_patch(patch: &[u8], files: &[FilePaint], opts: &Opts) -> Vec<u8> {
    diff_color::colorize_patch_ex(
        patch,
        &opts.colors,
        &PaintOptions {
            ws_error_highlight: opts.ws_error_highlight,
            indicators: opts.indicators,
            ..Default::default()
        },
        files,
        FilePaint::new(opts.ws_rule),
        &opts.extra,
    )
}

/// True when any byte is outside 7-bit ASCII — git's `has_non_ascii()`.
fn non_ascii(s: &str) -> bool {
    s.bytes().any(|b| b >= 0x80)
}

/// `Message-ID:` and the `In-Reply-To:`/`References:` chain, in the order
/// `log_write_email_headers()` prints them: straight after the mbox `From` line
/// and ahead of the pretty-printed identity headers.
fn write_message_ids(out: &mut Vec<u8>, th: &ThreadState, prefix: &str) -> Result<()> {
    if let Some(id) = &th.message_id {
        writeln!(out, "Message-ID: <{id}>")?;
        out.extend_from_slice(prefix.as_bytes());
    }
    if let Some(last) = th.refs.last() {
        writeln!(out, "In-Reply-To: <{last}>")?;
        for (i, r) in th.refs.iter().enumerate() {
            let lead = if i > 0 { "\t" } else { "References: " };
            writeln!(out, "{lead}<{r}>")?;
        }
        out.extend_from_slice(prefix.as_bytes());
    }
    Ok(())
}

/// The `multipart/mixed` header block and its first (text) part, appended to the
/// extra headers exactly as `log_write_email_headers()` builds it. The cover
/// letter is written with `maybe_multipart = 0`, so it never gets this.
fn write_mime_preamble(sb: &mut String, opts: &Opts) {
    let Some(b) = &opts.mime_boundary else {
        return;
    };
    sb.push_str(&format!(
        "MIME-Version: 1.0\n\
         Content-Type: multipart/mixed; boundary=\"{MIME_BOUNDARY_LEADER}{b}\"\n\
         \n\
         This is a multi-part message in MIME format.\n\
         --{MIME_BOUNDARY_LEADER}{b}\n\
         Content-Type: text/plain; charset=UTF-8; format=fixed\n\
         Content-Transfer-Encoding: 8bit\n\n"
    ));
}

/// git's `diffopt.stat_sep` under `--attach`/`--inline`: the MIME part that
/// carries the patch, emitted after the diffstat instead of a plain blank line.
fn mime_stat_sep(commit: &gix::Commit<'_>, nr: usize, opts: &Opts) -> Result<Option<String>> {
    let Some(b) = &opts.mime_boundary else {
        return Ok(None);
    };
    let name = patch_filename(commit, nr, opts)?;
    let disposition = if opts.no_inline { "attachment" } else { "inline" };
    Ok(Some(format!(
        "\n--{MIME_BOUNDARY_LEADER}{b}\n\
         Content-Type: text/x-patch; name=\"{name}\"\n\
         Content-Transfer-Encoding: 8bit\n\
         Content-Disposition: {disposition}; filename=\"{name}\"\n\n"
    )))
}

/// Everything git prints between the commit message and the patch.
///
/// Two ports meet here. `log_tree_diff()` (log-tree.c) writes the blank line
/// that separates log from diff, prefixing it with `---` only when the diffstat
/// and the patch are *both* being shown. `diff_flush()` (diff.c) then writes the
/// selected stat blocks in a fixed order and, if any of them set its `separator`
/// counter, one more blank line before the patch. Plain (non-`lines`) dirstat is
/// deliberately outside that counter in git, which is why `--dirstat` alone
/// leaves no blank line before the patch while `--dirstat=lines` does.
fn emit_stat_blocks(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    changes: &[ChangeDetached],
    stats: &[StatEntry],
    opts: &Opts,
    shown_dashes: bool,
    stat_sep: Option<&str>,
    dissimilarity: &HashMap<Vec<u8>, u32>,
    raw_abbrev: Abbrev,
    // `o->output_format`. Taken as an argument rather than read off [`Opts`] because
    // the combined form runs this pass under `STAT_FORMAT_MASK` alone
    // (combine-diff.c:1570), with the raw and patch bits masked off.
    output_format: u32,
    // Whether `DIFF_FORMAT_PATCH` is still set, which is the only thing that makes
    // `diff_flush()` write its separator after the stat block (diff.c:7228-7236).
    patch_follows: bool,
    // Byte ranges of `out` that `--line-prefix` must not touch, appended to.
    raw_spans: &mut Vec<(usize, usize)>,
) -> Result<()> {
    if !shown_dashes && output_format & FMT_DIFFSTAT != 0 {
        out.extend_from_slice(b"---");
    }
    out.push(b'\n');

    let dirstat_by_line = output_format & FMT_DIRSTAT != 0 && opts.dirstat.by_line;
    let mut separator = false;

    // `diff_flush()` runs the raw formats first, ahead of every stat block
    // (diff.c:7200-7217).
    if output_format & FMT_RAW != 0 {
        emit_raw(repo, out, changes, raw_abbrev, dissimilarity, opts)?;
        separator = true;
    }

    if output_format & (FMT_DIFFSTAT | FMT_NUMSTAT | FMT_SHORTSTAT) != 0 || dirstat_by_line {
        if output_format & FMT_NUMSTAT != 0 {
            emit_numstat(out, stats)?;
        }
        if output_format & FMT_DIFFSTAT != 0 {
            emit_stats(out, stats, stat_widths(opts), &opts.colors)?;
        }
        if output_format & FMT_SHORTSTAT != 0 {
            emit_stat_summary(out, stats)?;
        }
        if dirstat_by_line {
            emit_dirstat_by_line(out, stats, &opts.dirstat)?;
        }
        separator = true;
    }
    if output_format & FMT_DIRSTAT != 0 && !dirstat_by_line {
        emit_dirstat(repo, out, changes, &opts.dirstat)?;
    }
    if output_format & FMT_SUMMARY != 0 && !is_summary_empty(changes, dissimilarity) {
        emit_summary(out, changes, dissimilarity)?;
        separator = true;
    }

    if separator && patch_follows {
        out.push(b'\n');
        // `--attach`/`--inline` hang the patch off its own MIME part, announced
        // here; with no stat block at all git never emits it and the patch stays
        // inside the first (text) part.
        if let Some(sep) = stat_sep {
            // `DIFF_SYMBOL_STAT_SEP` is `fputs(o->stat_sep, o->file)` with no
            // `diff_line_prefix()` in front of it (diff.c:1673-1675), so the MIME
            // part header goes out unprefixed even inside a prefixed message.
            let start = out.len();
            out.extend_from_slice(sep.as_bytes());
            raw_spans.push((start, out.len()));
        }
    }
    Ok(())
}

/// Port of `make_cover_letter()` (log-tree.c): the placeholder subject and
/// blurb, a shortlog of the series, and the diffstat of the whole range.
///
/// The magic `From` line names `list[0]`, the first commit off the walk, and the
/// identity is the committer's — the cover letter is written now, by whoever
/// runs the command, not by the author of any one patch.
fn render_cover_letter(
    repo: &gix::Repository,
    commits: &[ObjectId],
    pending: &[ObjectId],
    total: usize,
    opts: &Opts,
    th: &ThreadState,
    out: &mut Vec<u8>,
) -> Result<std::result::Result<(), ExitCode>> {
    // `make_cover_letter()`'s `head = list[0]`: the first commit the walk handed
    // back. `cmd_format_patch` emits `list[]` backwards, so `list[0]` is always
    // the *last* patch of the printed series — including under `--reverse`, which
    // flips the walk itself and so flips both ends together.
    let head = *commits.last().expect("a non-empty series");
    write_from_line(out, head, opts)?;
    // `make_cover_letter()` reaches `log_write_email_headers()` but never
    // `show_log()`, so only the three `graph_show_oneline()` calls inside it write
    // `--line-prefix` — one after the magic `From` line and one after each
    // message-id block. Everything else the cover letter prints (`Date:`,
    // `Subject:`, the blurb, the shortlog) goes out unprefixed.
    let prefix = opts.line_prefix.as_deref().unwrap_or_default();
    out.extend_from_slice(prefix.as_bytes());
    write_message_ids(out, th, prefix)?;

    let mut sb = String::new();
    // `make_cover_letter()` writes `cfg->from ? cfg->from : git_committer_info(0)`
    // through `pp_user_info()`. An identity from `--from`/`format.from` carries
    // no timestamp, so its `Date:` is the epoch — git's own behaviour, since
    // `show_ident_date()` sees an unset date field.
    let (name, mail, date) = match &opts.from {
        Some(from) => (
            from.name.clone(),
            from.mail.clone(),
            gix::date::Time::new(0, 0).format(gix::date::time::format::GIT_RFC2822)?,
        ),
        None => match repo.committer().transpose()? {
            Some(sig) => (
                sig.name.to_str()?.to_owned(),
                sig.email.to_str()?.to_owned(),
                sig.time()?.format(gix::date::time::format::GIT_RFC2822)?,
            ),
            // No committer identity configured: fall back to the series' author
            // so the cover letter is still a well-formed message.
            None => {
                let commit = repo.find_object(head)?.try_into_commit()?;
                let author = commit.author()?;
                (
                    author.name.to_str()?.to_owned(),
                    author.email.to_str()?.to_owned(),
                    author.time()?.format(gix::date::time::format::GIT_RFC2822)?,
                )
            }
        },
    };
    write_identity_headers(&mut sb, &name, &mail, &date, opts.encode_email_headers);

    // `prepare_cover_text()`: the branch description, if there is one and
    // `--cover-from-description` did not switch it off, supplies the subject
    // and/or the blurb.
    let description = read_cover_description(repo, opts)?;
    let (subject, blurb) = cover_text(&description, opts);
    write_subject(&mut sb, &subject, 0, total, opts);
    // `make_cover_letter()` decides `need_8bit_cte` by scanning the *raw commit
    // buffers* of the whole series — the cover letter carries no message of its
    // own, so a non-ASCII byte anywhere in the series (identity lines included)
    // is what turns the 8-bit MIME block on, whether or not any header was
    // Q-encoded.
    let mut need_8bit = false;
    for id in commits {
        let commit = repo.find_object(*id)?.try_into_commit()?;
        if commit.data.iter().any(|&b| b >= 0x80) {
            need_8bit = true;
            break;
        }
    }
    if need_8bit {
        sb.push_str("MIME-Version: 1.0\n");
        sb.push_str(&format!("Content-Type: text/plain; charset={ENCODING}\n"));
        sb.push_str("Content-Transfer-Encoding: 8bit\n");
    }
    // The cover letter is written with `maybe_multipart = 0`, so it never gets
    // the `--attach`/`--inline` preamble the patches do.
    write_extra_headers(&mut sb, opts);
    sb.push('\n');
    let mut body: Vec<u8> = Vec::new();
    pp_remainder_tabs(&blurb, &mut body, opts.expand_tabs);
    sb.push_str(
        body.to_str()
            .map_err(|_| anyhow!("branch description is not valid UTF-8"))?,
    );
    // `fprintf(file, "%s\n", sb.buf)` — one more newline closes the header+blurb.
    sb.push('\n');
    out.extend_from_slice(sb.as_bytes());

    if let Err(code) = emit_commit_list(repo, commits, opts, out)? {
        return Ok(Err(code));
    }

    // "We can only do diffstat with a unique reference point": the block runs
    // against the walk's single boundary commit, and is skipped entirely when the
    // series has none or more than one — several roots, several bases, or a
    // `--no-walk` list whose commits do not share one parent.
    //
    // `show_diffstat()` (builtin/log.c) builds its own `diff_options` with a
    // hard-coded `DIFF_FORMAT_SUMMARY | DIFF_FORMAT_DIFFSTAT`, so the cover
    // letter keeps the stat+summary block whatever the series was asked for, and
    // closes it with a blank line even when the two trees turn out identical.
    if let Some(origin) = series_origin(repo, commits)? {
        // `--no-walk` makes `process_parents()` return before it parses any
        // parent, so a boundary commit that the revision arguments did not name
        // themselves reaches `show_diffstat()` unparsed: `get_commit_tree_oid()`
        // then hands `diff_tree_oid()` a NULL tree and the stat comes out as
        // everything in `head`'s tree rather than as the range's changes. A
        // boundary that *was* on the pending list — a merge the series skipped,
        // say — went through `handle_commit()` and has its tree.
        let unparsed = opts.no_walk && !pending.contains(&origin);
        let base = if unparsed {
            None
        } else {
            Some(repo.find_object(origin)?.try_into_commit()?.tree()?)
        };
        let head_tree = repo.find_object(head)?.try_into_commit()?.tree()?;
        let abbrev = index_abbrev(repo, &head_tree, opts)?;
        let mut dissimilarity = HashMap::new();
        let mut changes = tree_changes(
            repo,
            base.as_ref(),
            Some(&head_tree),
            opts.pathspec.as_ref(),
            opts.active_relative(),
            &RenameOpts::from_opts(opts),
            &mut dissimilarity,
        )?;
        rotate_changes(&mut changes, opts);
        apply_diff_filter(&mut changes, opts, &dissimilarity);
        let mut discard: Vec<u8> = Vec::new();
        let r = render_changes(repo, &mut discard, &changes, abbrev, opts, &dissimilarity)?;
        // `show_diffstat()` memcpy's `rev->diffopt`, keeping the width knobs, so
        // the cover letter's combined diffstat honors them too.
        // `show_diffstat()` goes through `diff_flush()`, so every row it writes
        // opens with `diff_line_prefix()`; the blank line after it does not.
        let stat_start = out.len();
        emit_stats(out, &r.stats, stat_widths(opts), &opts.colors)?;
        emit_summary(out, &r.kept, &dissimilarity)?;
        let stat_end = out.len();
        prefix_lines(out, stat_start, stat_end, prefix.as_bytes());
        out.push(b'\n');
    }
    Ok(Ok(()))
}

/// Port of `prepare_cover_text()` (builtin/log.c): read the branch description
/// that `--cover-from-description` will draw the subject and blurb from.
///
/// `--description-file` wins over `branch.<name>.description`, and
/// `--cover-from-description=none` skips the lookup entirely.
fn read_cover_description(repo: &gix::Repository, opts: &Opts) -> Result<Vec<u8>> {
    if opts.cover_from == CoverFrom::None {
        return Ok(Vec::new());
    }
    if let Some(path) = opts.description_file.as_deref().filter(|p| !p.is_empty()) {
        return std::fs::read(path)
            .map_err(|e| anyhow!("unable to read branch description file '{path}': {e}"));
    }
    let Some(branch) = opts.branch_name.as_deref().filter(|b| !b.is_empty()) else {
        return Ok(Vec::new());
    };
    let key = format!("branch.{branch}.description");
    Ok(repo
        .config_snapshot()
        .string(&key)
        .map(|v| v.to_vec())
        .unwrap_or_default())
}

/// The subject and blurb `prepare_cover_text()` settles on for the given mode.
fn cover_text(description: &[u8], opts: &Opts) -> (String, Vec<u8>) {
    let default = (COVER_SUBJECT.to_owned(), COVER_BLURB.as_bytes().to_vec());
    if opts.cover_from == CoverFrom::None || description.is_empty() {
        return default;
    }
    let (subject, rest) = format_subject(description);
    // `message` keeps the placeholder subject and uses the whole description as
    // the blurb; `subject` splits it; `auto` splits it only when the first
    // paragraph is short enough to read as a subject line.
    let split = match opts.cover_from {
        CoverFrom::Subject => true,
        CoverFrom::Auto => subject.len() <= COVER_FROM_AUTO_MAX_SUBJECT_LEN,
        _ => false,
    };
    if !split {
        return (default.0, description.to_vec());
    }
    match String::from_utf8(subject) {
        Ok(s) => (s, rest.to_vec()),
        Err(_) => default,
    }
}

/// Port of `make_cover_letter()`'s commit-list dispatch: `shortlog` (the
/// default), `modern`, a `log:<format>` prefix, or any bare format string that
/// contains a `%`. Anything else is `die("'%s' is not a valid format string")`.
fn emit_commit_list(
    repo: &gix::Repository,
    commits: &[ObjectId],
    opts: &Opts,
    out: &mut Vec<u8>,
) -> Result<std::result::Result<(), ExitCode>> {
    let fmt = opts.commit_list_format.as_deref().unwrap_or("shortlog");
    if let Some(rest) = fmt.strip_prefix("log:") {
        generate_commit_list(repo, commits, rest, out)?;
        return Ok(Ok(()));
    }
    match fmt {
        "shortlog" => {
            emit_shortlog(repo, commits, out)?;
            out.push(b'\n');
        }
        // git spells the built-in `modern` layout as a format string, so it
        // wraps at the mail width and numbers each entry within the series.
        "modern" => {
            let n = commits.len();
            for (i, id) in commits.iter().enumerate() {
                let commit = repo.find_object(*id)?.try_into_commit()?;
                let (subject, _) = format_subject(skip_blank_lines(commit.message_raw()?));
                let line = format!(
                    "[{}/{n}] {}",
                    i + 1,
                    subject
                        .to_str()
                        .map_err(|_| anyhow!("commit subject is not valid UTF-8"))?
                );
                let mut wrapped = String::new();
                wrap_text(&mut wrapped, &line, 0, 0, MAIL_DEFAULT_WRAP);
                out.extend_from_slice(wrapped.as_bytes());
                out.push(b'\n');
            }
            out.push(b'\n');
        }
        f if f.contains('%') => generate_commit_list(repo, commits, f, out)?,
        // git validates the spec only here, after the cover letter's headers and
        // blurb have already been written, so the partial message is kept.
        f => return Ok(Err(fatal(&format!("'{f}' is not a valid format string")))),
    }
    Ok(Ok(()))
}

/// Port of `generate_commit_list_cover()` (builtin/log.c): one line per commit,
/// oldest first, rendered through a `--pretty=format:` string.
///
/// git runs this with a zeroed `pretty_print_context`, so the abbreviating
/// placeholders answer with full object names — which is what `log`'s shared
/// `format_commit()` reproduces.
fn generate_commit_list(
    repo: &gix::Repository,
    commits: &[ObjectId],
    fmt: &str,
    out: &mut Vec<u8>,
) -> Result<()> {
    for id in commits {
        let commit = repo.find_object(*id)?.try_into_commit()?;
        out.extend_from_slice(&super::log::format_commit(repo, &commit, fmt)?);
        out.push(b'\n');
    }
    out.push(b'\n');
    Ok(())
}

/// git's shortlog as the cover letter embeds it: one `Name (count):` group per
/// author, most commits first, each subject indented by two spaces.
fn emit_shortlog(repo: &gix::Repository, commits: &[ObjectId], out: &mut Vec<u8>) -> Result<()> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for id in commits {
        let commit = repo.find_object(*id)?.try_into_commit()?;
        let author = commit.author()?.name.to_str()?.to_owned();
        let msg = skip_blank_lines(commit.message_raw()?);
        let (title, _) = format_subject(msg);
        let title = title.to_str()?.to_owned();
        match groups.iter_mut().find(|(name, _)| *name == author) {
            Some((_, subjects)) => subjects.push(title),
            None => groups.push((author, vec![title])),
        }
    }
    // Ties keep author order stable by name, as git's string list does.
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));

    for (i, (name, subjects)) in groups.iter().enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        writeln!(out, "{name} ({}):", subjects.len())?;
        for s in subjects {
            writeln!(out, "  {s}")?;
        }
    }
    Ok(())
}

/// The mbox magic line. `--zero-commit` replaces the commit name with zeroes.
fn write_from_line(out: &mut Vec<u8>, id: ObjectId, opts: &Opts) -> Result<()> {
    let name = if opts.zero_commit {
        ObjectId::null(id.kind()).to_hex().to_string()
    } else {
        id.to_hex().to_string()
    };
    writeln!(out, "From {name} Mon Sep 17 00:00:00 2001")?;
    Ok(())
}

/// `From:` — RFC2047 when non-ASCII, RFC822 quoting for specials, else wrapped —
/// followed by `Date:`.
///
/// `encode` is git's `encode_email_headers` (`--[no-]encode-email-headers`,
/// `format.encodeEmailHeaders`, default on). With it off the name goes out as raw
/// UTF-8 through the ordinary quoting/wrapping path; only the Q-encoding is
/// skipped, so the `MIME-Version:`/`Content-Transfer-Encoding: 8bit` block a
/// non-ASCII message still emits is unchanged.
pub(super) fn write_identity_headers(sb: &mut String, name: &str, mail: &str, date: &str, encode: bool) {
    sb.push_str("From: ");
    let mut max_length = HEADER_MAX_LENGTH;
    if encode && needs_rfc2047_encoding(name) {
        add_rfc2047(sb, name, true);
        max_length = 76;
    } else if name.bytes().any(is_rfc822_special) {
        let quoted = rfc822_quoted(name);
        wrap_text(sb, &quoted, -6, 1, max_length);
    } else {
        wrap_text(sb, name, -6, 1, max_length);
    }
    if max_length < last_line_length(sb) + 2 + mail.len() as i64 + 1 {
        sb.push('\n');
    }
    sb.push_str(&format!(" <{mail}>\n"));
    sb.push_str(&format!("Date: {date}\n"));
}

/// `Subject: [<prefix> n/total] <title>`, with the numbering git uses. Under
/// `-k`/`--keep-subject` the prefix and numbering are dropped entirely, so the
/// bare `Subject: <title>` carries the commit's own subject.
fn write_subject(sb: &mut String, title: &str, nr: usize, total: usize, opts: &Opts) {
    if opts.keep_subject {
        sb.push_str("Subject: ");
    } else if total > 0 {
        let width = diffstat::decimal_width(total as u64) as usize;
        let sep = if opts.subject_prefix.is_empty() {
            ""
        } else {
            " "
        };
        sb.push_str(&format!(
            "Subject: [{}{sep}{:0width$}/{total}] ",
            opts.subject_prefix, nr
        ));
    } else if !opts.subject_prefix.is_empty() {
        sb.push_str(&format!("Subject: [{}] ", opts.subject_prefix));
    } else {
        sb.push_str("Subject: ");
    }
    if opts.encode_email_headers && needs_rfc2047_encoding(title) {
        add_rfc2047(sb, title, false);
    } else {
        let consumed = -last_line_length(sb);
        wrap_text(sb, title, consumed, 1, HEADER_MAX_LENGTH);
    }
    sb.push('\n');
}

/// `format.mboxrd`: escape the message body's `/^>*From /` lines with one more
/// `>`, so a reader splitting an mbox on `From ` cannot mistake a body line for
/// a message separator. git models this as a distinct pretty format
/// (`CMIT_FMT_MBOXRD`) whose `pp_remainder()` prepends the `>`, which is why it
/// reaches the commit-message body only: the subject is a header, and every
/// diff line already carries a ` `/`+`/`-` prefix. The config is honoured only
/// with `--stdout`, exactly as git documents and implements it — patches written
/// to files are never concatenated into an mbox by format-patch itself.
fn mboxrd_escape(body: &mut Vec<u8>, opts: &Opts) {
    if !opts.pretty_mboxrd && !(opts.mboxrd && opts.to_stdout) {
        return;
    }
    // `pp_remainder()` reaches the `>` escape only through its plain branch:
    // `expand_tabs_in_log` takes an earlier one (pretty.c:2281-2293), so asking
    // for both silently drops the escaping. Reproduce that rather than combine
    // them.
    if opts.expand_tabs != 0 {
        return;
    }
    let mut out = Vec::with_capacity(body.len());
    for line in body.split_inclusive(|&b| b == b'\n') {
        if is_mboxrd_from(line) {
            out.push(b'>');
        }
        out.extend_from_slice(line);
    }
    *body = out;
}

/// git's `is_mboxrd_from()`: a line matching `/^>*From /`.
pub(super) fn is_mboxrd_from(line: &[u8]) -> bool {
    let rest = &line[line.iter().take_while(|&&b| b == b'>').count()..];
    rest.starts_with(b"From ")
}

/// `--add-header` lines (verbatim), then the `To:` and `Cc:` recipient lists,
/// emitted after the identity/MIME headers and before the blank line that ends
/// the header block. Each recipient list is folded one entry per continuation
/// line, aligned under the first address, the way git emits them.
fn write_extra_headers(sb: &mut String, opts: &Opts) {
    for h in &opts.add_header {
        sb.push_str(h);
        sb.push('\n');
    }
    write_recipient_list(sb, "To", &opts.to);
    write_recipient_list(sb, "Cc", &opts.cc);
}

fn write_recipient_list(sb: &mut String, name: &str, list: &[String]) {
    if list.is_empty() {
        return;
    }
    sb.push_str(name);
    sb.push_str(": ");
    let indent = " ".repeat(name.len() + 2);
    for (idx, value) in list.iter().enumerate() {
        if idx > 0 {
            sb.push_str(",\n");
            sb.push_str(&indent);
        }
        sb.push_str(value);
    }
    sb.push('\n');
}

// ---------------------------------------------------------------------------
// Threading, notes and base info
// ---------------------------------------------------------------------------

/// Port of `gen_message_id()` (builtin/log.c): `<base>.<epoch>.git.<committer
/// e-mail>`.
///
/// The timestamp is `time(NULL)` at the moment the id is minted, so a generated
/// `Message-ID` is by construction not reproducible across runs — git's own ids
/// differ between two invocations a second apart.
fn gen_message_id(repo: &gix::Repository, base: &str) -> Result<String> {
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Ok(format!("{base}.{epoch}.git.{}", committer_ident(repo)?.mail))
}

/// The `base-commit:`/`prerequisite-patch-id:` trailer block, as
/// `struct base_tree_info` holds it.
struct Bases {
    base: ObjectId,
    /// Patch ids in collection order; `print_bases()` emits them reversed.
    patch_ids: Vec<ObjectId>,
}

/// Port of `get_base_commit()` + `prepare_bases()` (builtin/log.c).
///
/// The `Err(ExitCode)` arm carries git's own `die()` message, already on stderr;
/// `Ok(None)` is "no base information was requested", which is also what
/// `format.useAutoBase=whenAble` degrades to when no base can be derived.
#[allow(clippy::type_complexity)]
fn resolve_bases(
    repo: &gix::Repository,
    commits: &[ObjectId],
    opts: &Opts,
) -> Result<std::result::Result<Option<Bases>, ExitCode>> {
    let (auto_select, die_on_failure) = match opts.auto_base {
        AutoBase::Never => match &opts.base_commit {
            Some(_) => (false, true),
            None => return Ok(Ok(None)),
        },
        AutoBase::Always => (true, true),
        AutoBase::WhenAble => (true, false),
    };
    // `die_on_failure` decides whether an underivable base is fatal or silently
    // drops the whole block.
    macro_rules! give_up {
        ($msg:expr) => {
            return Ok(if die_on_failure {
                Err(fatal($msg))
            } else {
                Ok(None)
            })
        };
    }

    let base = if !auto_select {
        let spec = opts.base_commit.as_deref().expect("a base was given");
        match repo
            .rev_parse_single(BStr::new(spec))
            .ok()
            .and_then(|id| id.object().ok())
            .and_then(|o| o.peel_to_commit().ok())
        {
            Some(c) => c.id,
            None => return Ok(Err(fatal(&format!("unknown commit {spec}")))),
        }
    } else {
        let upstream = repo
            .head_ref()?
            .map(|r| r.name().to_owned())
            .and_then(|name| repo.branch_remote_tracking_ref_name(name.as_ref(), gix::remote::Direction::Fetch).and_then(|r| r.ok()));
        let Some(upstream) = upstream else {
            give_up!(
                "failed to get upstream, if you want to record base commit automatically,\n\
                 please use git branch --set-upstream-to to track a remote branch.\n\
                 Or you could specify base commit by --base=<base-commit-id> manually"
            );
        };
        let Some(tip) = repo
            .find_reference(upstream.as_ref())
            .ok()
            .and_then(|mut r| r.peel_to_id().ok())
            .map(|id| id.detach())
        else {
            give_up!(&format!("failed to resolve '{upstream}' as a valid ref"));
        };
        // "There should be one and only one merge base."
        let mut bases = repo.merge_bases_many(tip, commits)?;
        if bases.len() != 1 {
            give_up!("could not find exact merge base");
        }
        bases.pop().expect("exactly one merge base").detach()
    };

    // Reduce the series to a single merge base, pairwise, the way git does.
    let mut rev: Vec<ObjectId> = commits.to_vec();
    while rev.len() > 1 {
        let mut next = Vec::with_capacity(rev.len().div_ceil(2));
        for pair in rev.chunks(2) {
            if pair.len() == 1 {
                next.push(pair[0]);
                continue;
            }
            let mut mb = repo.merge_bases_many(pair[0], &pair[1..2])?;
            if mb.len() != 1 {
                give_up!("failed to find exact merge base");
            }
            next.push(mb.pop().expect("exactly one merge base").detach());
        }
        rev = next;
    }
    let tip = rev[0];
    let is_ancestor = repo
        .merge_bases_many(base, &[tip])?
        .iter()
        .any(|b| b.detach() == base);
    if !is_ancestor {
        give_up!("base commit should be the ancestor of revision list");
    }
    if commits.contains(&base) {
        give_up!("base commit shouldn't be in revision list");
    }

    // `prepare_bases()`: every non-merge commit between the base and the series
    // that is not itself part of the series is a prerequisite.
    let in_series: std::collections::HashSet<ObjectId> = commits.iter().copied().collect();
    let mut patch_ids = Vec::new();
    for info in repo
        .rev_walk(commits.iter().copied())
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
        .with_hidden(Some(base))
        .all()?
    {
        let info = info?;
        if in_series.contains(&info.id) {
            continue;
        }
        let commit = repo.find_object(info.id)?.try_into_commit()?;
        if commit.parent_ids().count() > 1 {
            continue;
        }
        patch_ids.push(commit_patch_id(repo, &commit)?);
    }
    Ok(Ok(Some(Bases { base, patch_ids })))
}

/// Port of `print_bases()` (builtin/log.c). Emitted once per series, on the
/// cover letter when there is one and otherwise on the first patch.
fn print_bases(out: &mut Vec<u8>, bases: &Bases) {
    out.extend_from_slice(format!("\nbase-commit: {}\n", bases.base).as_bytes());
    for id in bases.patch_ids.iter().rev() {
        out.extend_from_slice(format!("prerequisite-patch-id: {id}\n").as_bytes());
    }
}

/// Port of `commit_patch_id()` (patch-ids.c) plus `diff_get_patch_id()`
/// (diff.c) in its unstable form — the one `prepare_bases()` asks for.
///
/// The hash is fed a canonical, header-only rendering of each file pair
/// (`diff--git`, `a/`+`b/` paths with whitespace removed, mode words) followed
/// by the raw diff lines, and each file's digest is folded into the running
/// result with `flush_one_hunk()`'s carrying byte-wise sum.
fn commit_patch_id(repo: &gix::Repository, commit: &gix::Commit<'_>) -> Result<ObjectId> {
    let kind = repo.object_hash();
    let new_tree = commit.tree()?;
    let old_tree = match commit.parent_ids().next() {
        Some(pid) => Some(pid.object()?.try_into_commit()?.tree()?),
        None => None,
    };
    let changes = tree_changes(
        repo,
        old_tree.as_ref(),
        Some(&new_tree),
        None,
        None,
        &RenameOpts::default(),
        &mut HashMap::new(),
    )?;

    let mut result = vec![0u8; kind.len_in_bytes()];
    let mut ctx = gix::hash::hasher(kind);
    for change in &changes {
        let path = change_path(change);
        let one = remove_space(path);
        let two = one.clone();
        let (old_id, new_id, old_mode, new_mode) = pair_info(change);

        ctx.update(b"diff--git");
        ctx.update(b"a/");
        ctx.update(&one);
        ctx.update(b"b/");
        ctx.update(&two);

        if old_mode == 0 {
            ctx.update(b"newfilemode");
            ctx.update(format!("{new_mode:06o}").as_bytes());
        } else if new_mode == 0 {
            ctx.update(b"deletedfilemode");
            ctx.update(format!("{old_mode:06o}").as_bytes());
        } else if old_mode != new_mode {
            ctx.update(b"oldmode");
            ctx.update(format!("{old_mode:06o}").as_bytes());
            ctx.update(b"newmode");
            ctx.update(format!("{new_mode:06o}").as_bytes());
        }

        let old_content = match old_id {
            Some((id, is_sub)) => content_of(repo, id, is_sub)?,
            None => Vec::new(),
        };
        let new_content = match new_id {
            Some((id, is_sub)) => content_of(repo, id, is_sub)?,
            None => Vec::new(),
        };
        if is_binary(&old_content) || is_binary(&new_content) {
            // A binary pair contributes its two object names instead of a diff.
            let hex = |o: Option<(ObjectId, bool)>| {
                o.map_or_else(|| ObjectId::null(kind), |(id, _)| id)
                    .to_hex()
                    .to_string()
            };
            ctx.update(hex(old_id).as_bytes());
            ctx.update(hex(new_id).as_bytes());
        } else {
            if old_mode == 0 {
                ctx.update(b"---/dev/null");
                ctx.update(b"+++b/");
                ctx.update(&two);
            } else if new_mode == 0 {
                ctx.update(b"---a/");
                ctx.update(&one);
                ctx.update(b"+++/dev/null");
            } else {
                ctx.update(b"---a/");
                ctx.update(&one);
                ctx.update(b"+++b/");
                ctx.update(&two);
            }
            let input = InternedInput::new(old_content.as_slice(), new_content.as_slice());
            // `xecfg.ctxlen = 3`, `xecfg.flags = XDL_EMIT_NO_HUNK_HDR`, and
            // Myers regardless of `--diff-algorithm`, because `prepare_bases()`
            // builds its own `diff_options`.
            //
            // git passes `xpp.flags = 0` here, i.e. `xdl_change_compact()`
            // without the indent heuristic, where the rendered patch uses it.
            // imara-diff exposes only the one post-processing pass, so an input
            // whose hunks the two compactions place differently would hash
            // differently; no such input turned up in differential testing.
            let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
            UnifiedDiff::new(
                &diff,
                &input,
                PatchIdWriter { ctx: &mut ctx },
                ContextSize::symmetrical(3),
            )
            .consume()?;
        }
        flush_one_hunk(&mut result, &mut ctx, kind);
    }
    Ok(ObjectId::from_bytes_or_panic(&result))
}

/// Hashes the diff body the way `patch_id_consume()` does: every line with its
/// `+`/`-`/` ` prefix and a newline, and no hunk headers
/// (`XDL_EMIT_NO_HUNK_HDR`) or `\ No newline` markers.
struct PatchIdWriter<'a> {
    ctx: &'a mut gix::hash::Hasher,
}
impl ConsumeHunk for PatchIdWriter<'_> {
    type Out = ();

    fn consume_hunk(
        &mut self,
        _header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        for &(kind, content) in lines {
            let mut line = Vec::with_capacity(content.len() + 2);
            line.push(match kind {
                DiffLineKind::Context => b' ',
                DiffLineKind::Add => b'+',
                DiffLineKind::Remove => b'-',
            });
            line.extend_from_slice(content);
            // xdiff's incomplete-line record still ends the hashed line with a
            // newline; only the `\ No newline` marker itself is skipped.
            if !content.ends_with(b"\n") {
                line.push(b'\n');
            }
            // `patch_id_consume()` strips whitespace before hashing, which is
            // what makes a patch id insensitive to indentation-only rewrites.
            self.ctx.update(&remove_space(&line));
        }
        Ok(())
    }

    fn finish(self) {}
}

/// Port of `flush_one_hunk()` (diff.c): fold this file's digest into the
/// running result as a byte-wise sum with carry, and restart the context.
fn flush_one_hunk(result: &mut [u8], ctx: &mut gix::hash::Hasher, kind: gix::hash::Kind) {
    let digest = std::mem::replace(ctx, gix::hash::hasher(kind)).try_finalize();
    let digest = digest.expect("a hash context always finalizes").as_slice().to_vec();
    let mut carry: u16 = 0;
    for i in 0..result.len() {
        carry += u16::from(result[i]) + u16::from(digest[i]);
        result[i] = carry as u8;
        carry >>= 8;
    }
}

/// The `(pre-image, post-image, old mode, new mode)` of one change, in the
/// shape `diff_get_patch_id()` reads a `diff_filepair` — a missing side is mode
/// `0`, which is how git spells creation and deletion there.
#[allow(clippy::type_complexity)]
fn pair_info(
    change: &ChangeDetached,
) -> (
    Option<(ObjectId, bool)>,
    Option<(ObjectId, bool)>,
    u32,
    u32,
) {
    match change {
        ChangeDetached::Addition {
            entry_mode, id, ..
        } => (
            None,
            Some((*id, entry_mode.is_commit())),
            0,
            entry_mode.value().into(),
        ),
        ChangeDetached::Deletion {
            entry_mode, id, ..
        } => (
            Some((*id, entry_mode.is_commit())),
            None,
            entry_mode.value().into(),
            0,
        ),
        ChangeDetached::Modification {
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
            ..
        } => (
            Some((*previous_id, previous_entry_mode.is_commit())),
            Some((*id, entry_mode.is_commit())),
            previous_entry_mode.value().into(),
            entry_mode.value().into(),
        ),
        // Never produced: rewrite tracking is off via Options::default().
        ChangeDetached::Rewrite { .. } => (None, None, 0, 0),
    }
}

/// git's `buffer_is_binary()`: a NUL in the first 8000 bytes.
fn is_binary(content: &[u8]) -> bool {
    content[..content.len().min(8000)].contains(&0)
}

/// Port of `remove_space()` (diff.c): git's own `isspace` class removed from a
/// path before it is hashed.
/// git's `isspace` is `sane_ctype`'s `GIT_SPACE`, which covers only space, tab,
/// newline and carriage return — vertical tab and form feed are *kept*.
fn remove_space(path: &[u8]) -> Vec<u8> {
    path.iter()
        .copied()
        .filter(|c| !matches!(c, b' ' | b'\t' | b'\n' | b'\r'))
        .collect()
}

fn write_signature(out: &mut Vec<u8>, opts: &Opts) {
    if opts.signature.is_empty() {
        return;
    }
    out.extend_from_slice(b"-- \n");
    out.extend_from_slice(opts.signature.as_bytes());
    if !opts.signature.ends_with('\n') {
        out.push(b'\n');
    }
    out.push(b'\n');
}

/// The file-level changes between two trees, in path order.
///
/// `tree_with_rewrites` reports the directory entry *and* its recursed contents;
/// git's patch format only names blobs and submodules, so tree entries are
/// dropped — keeping one would render a raw tree object as a binary file.
///
/// A `-- <pathspec>` is applied here, before rename detection, because git
/// applies it inside `diff_tree_oid()`'s walk (`tree_entry_interesting()`) and
/// only then runs `diffcore_std()`. So a rename whose other half the pathspec
/// excludes never becomes a pair: `format-patch -- old` over a `old` → `new`
/// rename reports a plain deletion.
fn tree_changes(
    repo: &gix::Repository,
    old_tree: Option<&gix::Tree<'_>>,
    new_tree: Option<&gix::Tree<'_>>,
    specs: Option<&super::log::PathspecMatcher>,
    relative: Option<&str>,
    ro: &RenameOpts,
    dissimilarity: &mut HashMap<Vec<u8>, u32>,
) -> Result<Vec<ChangeDetached>> {
    let mut changes =
        repo.diff_tree_to_tree(old_tree, new_tree, gix::diff::Options::default())?;
    changes.retain(|c| !is_tree_entry(c));
    if let Some(specs) = specs {
        changes.retain(|c| specs.matches(change_path(c)));
    }
    // `--relative`'s *narrowing* half is `diff_queue()`'s prefix test (diff.c:7630),
    // which runs while the queue is being built — before `diffcore_rename()`. That
    // ordering is visible: measured against stock 2.55.0, `--relative=src` over a
    // commit that renamed `old/moved.txt` to `src/moved.txt` reports a plain
    // creation, because the deletion side never entered the queue to be paired.
    if let Some(prefix) = relative {
        changes.retain(|c| change_path(c).starts_with(prefix.as_bytes()));
    }
    changes.sort_by(|a, b| change_path(a).cmp(change_path(b)));
    let mut unchanged = if ro.find_copies_harder {
        shared_blobs(repo, old_tree, &changes, specs)?
    } else {
        Vec::new()
    };
    // The unmodified pairs `--find-copies-harder` feeds in as copy sources are queued
    // through the very same `diff_queue()` (tree-diff.c:519), so the prefix test
    // applies to them too — without it a copy could be found from a source the
    // narrowing had already excluded.
    if let Some(prefix) = relative {
        unchanged.retain(|(path, _, _)| path.starts_with(prefix.as_bytes()));
    }
    detect_renames(repo, &mut changes, ro, &unchanged, dissimilarity)?;
    Ok(changes)
}

/// The blobs the two trees have in common, which `--find-copies-harder` needs in
/// the queue as copy sources (tree-diff.c:519, :557). The changed paths are
/// already pairs of their own, so only the pre-image entries the diff did *not*
/// report are collected.
fn shared_blobs(
    repo: &gix::Repository,
    old_tree: Option<&gix::Tree<'_>>,
    changes: &[ChangeDetached],
    specs: Option<&super::log::PathspecMatcher>,
) -> Result<Vec<(Vec<u8>, u32, ObjectId)>> {
    let Some(tree) = old_tree else {
        return Ok(Vec::new());
    };
    let touched: HashSet<&[u8]> = changes.iter().map(change_path).collect();
    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse().breadthfirst(&mut recorder)?;
    let mut out = Vec::new();
    for entry in recorder.records {
        if entry.mode.is_tree() || entry.mode.is_commit() {
            continue;
        }
        let path = entry.filepath.to_vec();
        if touched.contains(path.as_slice()) {
            continue;
        }
        if specs.is_some_and(|s| !s.matches(&path)) {
            continue;
        }
        out.push((path, u32::from(entry.mode.value()), entry.oid));
    }
    Ok(out)
}

/// The `diffcore_std()` detection settings one tree diff runs under, split out of
/// [`Opts`] because `diff_get_patch_id()` builds its own `diff_options` from
/// `diff_setup()` and so never sees the command line's `-M`/`-C`/`-B`.
#[derive(Clone, Copy)]
struct RenameOpts {
    detect_rename: Option<u8>,
    rename_score: u32,
    find_copies_harder: bool,
    rename_empty: bool,
    break_opt: i64,
    rename_limit: Option<i64>,
}

impl Default for RenameOpts {
    fn default() -> Self {
        RenameOpts {
            detect_rename: None,
            rename_score: 0,
            find_copies_harder: false,
            rename_empty: true,
            break_opt: -1,
            rename_limit: None,
        }
    }
}

impl RenameOpts {
    fn from_opts(o: &Opts) -> Self {
        RenameOpts {
            detect_rename: o.detect_rename,
            rename_score: o.rename_score,
            find_copies_harder: o.find_copies_harder,
            rename_empty: o.rename_empty,
            break_opt: o.break_opt,
            rename_limit: o.rename_limit,
        }
    }
}

/// Port of `diffcore_rotate()` (diff.c), which `diffcore_std()` runs over the
/// queue before any format sees it — so the diffstat, the summary, the dirstat
/// and the patch all agree on the new order.
///
/// `--rotate-to=<path>` moves the pair naming `<path>` to the front and sends the
/// pairs that stood before it to the back; `--skip-to=<path>` drops them instead.
///
/// The anchor is the first pair whose name sorts at or *after* `<path>`, not an
/// exact match: a path the commit did not touch still splits the queue where it
/// would have stood. When every name sorts before it there is no anchor at all
/// and the queue is left alone — only `git diff` sets `rotate_to_strict`, so
/// nothing here dies. Both halves verified against stock git 2.55.0 over a commit
/// touching `del.txt`, `f.txt` and `new.txt`: `format-patch --skip-to=ig.txt`
/// prints `new.txt` alone, while `--skip-to=nope` prints all three.
/// `diffcore_apply_filter()`, which `diffcore_std()` runs last — after break,
/// rename, pickaxe, order and rotate (diff.c:7504-7526).
fn apply_diff_filter(
    changes: &mut Vec<ChangeDetached>,
    opts: &Opts,
    dissimilarity: &HashMap<Vec<u8>, u32>,
) {
    let Some(filter) = opts.diff_filter else {
        return;
    };
    let pairs: Vec<(u8, Option<u32>)> = changes
        .iter()
        .map(|c| pair_status(c, dissimilarity))
        .collect();
    let mut keep = super::diff_filter::apply(filter, &pairs).into_iter();
    changes.retain(|_| keep.next().unwrap_or(true));
}

/// One pair's `p->status` and `p->score`, the two fields `match_filter()` reads.
/// The score is `-B`'s dissimilarity, which is the only thing that makes a
/// modified pair "broken".
fn pair_status(change: &ChangeDetached, dissimilarity: &HashMap<Vec<u8>, u32>) -> (u8, Option<u32>) {
    match change {
        ChangeDetached::Addition { .. } => (b'A', None),
        ChangeDetached::Deletion { .. } => (b'D', None),
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            entry_mode,
            ..
        } => {
            // `DIFF_PAIR_TYPE_CHANGED()`: the file-type bits differ.
            let changed_type = (u32::from(previous_entry_mode.value())
                ^ u32::from(entry_mode.value()))
                & 0o170000
                != 0;
            (
                if changed_type { b'T' } else { b'M' },
                dissimilarity.get(location.as_slice()).copied(),
            )
        }
        ChangeDetached::Rewrite { copy, .. } => (if *copy { b'C' } else { b'R' }, None),
    }
}

fn rotate_changes(changes: &mut Vec<ChangeDetached>, opts: &Opts) {
    let Some((skip, path)) = &opts.skip_or_rotate else {
        return;
    };
    let Some(at) = changes
        .iter()
        .position(|c| change_path(c) >= path.as_slice())
    else {
        return;
    };
    let head: Vec<ChangeDetached> = changes.drain(..at).collect();
    if !skip {
        changes.extend(head);
    }
}

/// Render every queued change into `patch`, returning the changes and the stat
/// rows the output formats should be shown.
///
/// Once a whitespace option or `-I<re>` is in force git decides the queue from
/// the rendered body (`diff_setup_done()`'s `diff_from_contents`): an in-place
/// edit whose diff came out empty is dropped from *every* format, while an
/// addition, a deletion, a rename and a mode change stay — none of those had the
/// body as their only reason to appear.
///
/// Observed against stock git 2.55.0 over a commit that mixes the shapes: with
/// `-b`, `git diff --raw`, `--name-status`, `--stat` and the patch all omit the
/// whitespace-only edit, while the same commit's mode change still prints
/// ` mode.txt | 0` and a rename carrying a whitespace-only edit still prints
/// ` ren.txt => ren2.txt | 0` plus a body-less `diff --git` block.
fn render_changes(
    repo: &gix::Repository,
    patch: &mut Vec<u8>,
    changes: &[ChangeDetached],
    abbrev: Abbrev,
    opts: &Opts,
    dissimilarity: &HashMap<Vec<u8>, u32>,
) -> Result<Rendered> {
    let from_contents = opts.ws != super::diff_pairs::Whitespace::Keep
        || !opts.ignore_regex.is_empty()
        || opts.ignore_blank_lines;
    let mut kept: Vec<ChangeDetached> = Vec::with_capacity(changes.len());
    let mut stats: Vec<StatEntry> = Vec::with_capacity(changes.len());
    // The plain patch accumulated since the last flush, and the per-file paint state
    // that indexes it. A `--submodule=log|diff` section is written already painted,
    // so the run before it has to be flushed to keep the order — the same drain
    // `git diff` performs at diff.rs:2568.
    let mut plain: Vec<u8> = Vec::new();
    let mut files: Vec<FilePaint> = Vec::with_capacity(changes.len());
    for change in changes {
        let mut one: Vec<u8> = Vec::new();
        let (stat, paint) = emit_change(repo, &mut one, change, abbrev, opts, dissimilarity)?;
        if from_contents && stat.added == 0 && stat.deleted == 0 && is_plain_edit(change) {
            continue;
        }
        // `builtin_diff()`'s submodule branch (diff.c:3870) replaces the whole
        // `diff --git` section — header included — with the submodule's own report.
        // The diffstat is computed by `builtin_diffstat()`, which has no such branch,
        // so the row this pair contributes is still the `Subproject commit` one.
        if opts.submodule_format != super::diff::SubmoduleFormat::Short
            && is_gitlink_pair(change)
        {
            patch.extend_from_slice(&paint_patch(&plain, &files, opts));
            plain.clear();
            files.clear();
            render_submodule_section(patch, repo, change, opts);
        } else {
            plain.extend_from_slice(&one);
            files.push(paint);
        }
        stats.push(stat);
        kept.push(change.clone());
    }
    patch.extend_from_slice(&paint_patch(&plain, &files, opts));
    Ok(Rendered { kept, stats })
}

/// What one commit's file-pair queue rendered into: the pairs that survived
/// `diff_from_contents` suppression and their diffstat rows.
struct Rendered {
    kept: Vec<ChangeDetached>,
    stats: Vec<StatEntry>,
}

/// `S_ISGITLINK()` on both sides of the pair, with an absent side not objecting —
/// the test `builtin_diff()` makes before it diverts to the submodule renderers.
fn is_gitlink_pair(change: &ChangeDetached) -> bool {
    match change {
        ChangeDetached::Addition { entry_mode, .. }
        | ChangeDetached::Deletion { entry_mode, .. } => entry_mode.is_commit(),
        ChangeDetached::Modification {
            previous_entry_mode,
            entry_mode,
            ..
        } => previous_entry_mode.is_commit() && entry_mode.is_commit(),
        ChangeDetached::Rewrite {
            source_entry_mode,
            entry_mode,
            ..
        } => source_entry_mode.is_commit() && entry_mode.is_commit(),
    }
}

/// `show_submodule_diff_summary()` / `show_submodule_inline_diff()` for one gitlink
/// pair, through the same implementations `git diff --submodule=<format>` uses.
fn render_submodule_section(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    change: &ChangeDetached,
    opts: &Opts,
) {
    let null = repo.object_hash().null();
    let (one, two) = match change {
        ChangeDetached::Addition { id, .. } => (null, *id),
        ChangeDetached::Deletion { id, .. } => (*id, null),
        ChangeDetached::Modification { previous_id, id, .. } => (*previous_id, *id),
        ChangeDetached::Rewrite { source_id, id, .. } => (*source_id, *id),
    };
    let path = gix::bstr::BString::from(change_path(change).to_vec());
    let abbrev = crate::abbrev::configured_abbrev(repo, repo.object_hash().len_in_hex());
    // A tree-to-tree pair never carries `two->dirty_submodule`, which only a
    // worktree comparison sets.
    if opts.submodule_format == super::diff::SubmoduleFormat::Log {
        super::diff_pairs::show_submodule_diff_summary(
            out, repo, &path, &one, &two, 0, abbrev, &opts.colors,
        );
        return;
    }
    let (a, b) = prefixes(opts);
    super::diff::submodule_inline_section(
        out,
        repo,
        &path,
        &one,
        &two,
        0,
        abbrev,
        &opts.colors,
        a.as_bytes(),
        b.as_bytes(),
        repo.object_hash(),
    );
}

/// An in-place content edit — both sides present, same path, same mode. It is the
/// one change shape whose only reason to be in the queue is its diff body.
fn is_plain_edit(change: &ChangeDetached) -> bool {
    matches!(
        change,
        ChangeDetached::Modification {
            previous_entry_mode,
            entry_mode,
            ..
        } if previous_entry_mode.value() == entry_mode.value()
    )
}

/// `diffcore_rename()`: replace each deletion/addition pair whose content matches (or
/// is similar enough) with the single rewrite entry git reports.
///
/// `format-patch` is a porcelain, so detection is on unless `diff.renames` turns it
/// off, at git's default 50%. The pass is the same port `git diff` uses, so the
/// pairing and the similarity index agree between them.
fn detect_renames(
    repo: &gix::Repository,
    changes: &mut Vec<ChangeDetached>,
    o: &RenameOpts,
    unchanged: &[(Vec<u8>, u32, ObjectId)],
    dissimilarity: &mut HashMap<Vec<u8>, u32>,
) -> Result<()> {
    use gix::bstr::ByteSlice;

    let cfg = repo.config_snapshot();
    // `diff_setup_done()` promotes detection to `DIFF_DETECT_COPY` whenever
    // `--find-copies-harder` is on, whatever `-M`/`--no-renames` asked for
    // (diff.c:5288-5289).
    let detect = if o.find_copies_harder {
        super::diffcore_rename::DETECT_COPY
    } else {
        o.detect_rename.unwrap_or_else(|| {
            super::diffcore_rename::config_rename(
                cfg.string("diff.renames").as_deref().map(|v| v.as_bstr()),
            )
        })
    };
    // A plain rename pass has nothing to pair up without both a source and a
    // destination. Copy detection also uses in-place edits as sources, and `-B`
    // runs before detection at all, so neither may take this shortcut.
    let has_pair = changes
        .iter()
        .any(|c| matches!(c, ChangeDetached::Addition { .. }))
        && changes
            .iter()
            .any(|c| matches!(c, ChangeDetached::Deletion { .. }));
    let skippable = detect != super::diffcore_rename::DETECT_COPY && o.break_opt == -1;
    if (detect == 0 && o.break_opt == -1) || (skippable && !has_pair) {
        return Ok(());
    }
    let opts = super::diffcore_rename::Options {
        detect_rename: detect,
        rename_score: o.rename_score,
        find_copies_harder: o.find_copies_harder,
        rename_empty: o.rename_empty,
        break_opt: o.break_opt,
        rename_limit: o.rename_limit.unwrap_or_else(|| {
            cfg.integer("diff.renameLimit")
                .unwrap_or(super::diffcore_rename::DEFAULT_RENAME_LIMIT)
        }),
        hash_kind: repo.object_hash(),
    };

    let null = gix::hash::ObjectId::null(repo.object_hash());
    let mut q = super::diffcore_rename::Queue::default();
    for change in changes.iter() {
        let (path, old, new, status) = match change {
            ChangeDetached::Addition {
                location,
                entry_mode,
                id,
                ..
            } => (location, None, Some((*id, entry_mode.value())), b'A'),
            ChangeDetached::Deletion {
                location,
                entry_mode,
                id,
                ..
            } => (location, Some((*id, entry_mode.value())), None, b'D'),
            ChangeDetached::Modification {
                location,
                previous_entry_mode,
                previous_id,
                entry_mode,
                id,
            } => (
                location,
                Some((*previous_id, previous_entry_mode.value())),
                Some((*id, entry_mode.value())),
                b'M',
            ),
            // gix never produces these here: the tree diff runs without rewrites.
            ChangeDetached::Rewrite { .. } => return Ok(()),
        };
        let one = q.add_spec(super::diffcore_rename::FileSpec::new(
            path.clone(),
            old.map_or(0, |(_, mode)| u32::from(mode)),
            old.map_or(null, |(id, _)| id),
            old.is_some(),
        ));
        let two = q.add_spec(super::diffcore_rename::FileSpec::new(
            path.clone(),
            new.map_or(0, |(_, mode)| u32::from(mode)),
            new.map_or(null, |(id, _)| id),
            new.is_some(),
        ));
        let idx = q.add_pair(one, two);
        q.pairs[idx].status = status;
    }
    // `--find-copies-harder` is a *tree-diff* flag before it is a rename one:
    // `diff_tree_paths()` stops skipping entries the two trees share
    // (tree-diff.c:519, :557), so every unchanged file reaches the queue as an
    // unmodified pair and `diffcore_rename()` can use it as a copy source. Such a
    // pair is `diff_unmodified_pair()` and is dropped again below.
    for (path, mode, id) in unchanged {
        let one = q.add_spec(super::diffcore_rename::FileSpec::new(
            path.clone().into(),
            *mode,
            *id,
            true,
        ));
        let two = q.add_spec(super::diffcore_rename::FileSpec::new(
            path.clone().into(),
            *mode,
            *id,
            true,
        ));
        let idx = q.add_pair(one, two);
        q.pairs[idx].status = b'M';
    }

    let mut content = super::diffcore_rename::OdbContent { repo };
    super::diffcore_rename::run(&mut q, &opts, &mut content);
    super::diffcore_rename::resolve_rename_copy(&mut q);

    let mut rebuilt: Vec<ChangeDetached> = Vec::with_capacity(q.pairs.len());
    for pair in &q.pairs {
        let source = &q.specs[pair.one];
        let dest = &q.specs[pair.two];
        let status = if pair.status == 0 { b'M' } else { pair.status };
        if !matches!(status, b'R' | b'C') {
            // Not a rename or a copy. `-B` can leave two pairs standing on one
            // path (a broken rewrite whose halves nothing re-paired), so the shape
            // is read back out of the queue rather than looked up by name.
            let change = match status {
                b'A' => ChangeDetached::Addition {
                    location: dest.path.clone(),
                    relation: None,
                    entry_mode: mode_from_octal(dest.mode),
                    id: dest.oid,
                },
                b'D' => ChangeDetached::Deletion {
                    location: source.path.clone(),
                    relation: None,
                    entry_mode: mode_from_octal(source.mode),
                    id: source.oid,
                },
                // An unmodified pair only exists because `--find-copies-harder`
                // put it there as a copy source; `diff_unmodified_pair()` keeps it
                // out of every output format.
                _ if source.oid == dest.oid && source.mode == dest.mode => continue,
                _ => ChangeDetached::Modification {
                    location: dest.path.clone(),
                    previous_entry_mode: mode_from_octal(source.mode),
                    previous_id: source.oid,
                    entry_mode: mode_from_octal(dest.mode),
                    id: dest.oid,
                },
            };
            // `-B`'s leftover score on a modification is the `dissimilarity index`
            // line git prints in place of a `similarity index` (diff.c:4897-4903).
            if status == b'M' && pair.score != 0 {
                dissimilarity.insert(
                    dest.path.to_vec(),
                    super::diffcore_rename::similarity_index(pair.score),
                );
            }
            rebuilt.push(change);
            continue;
        }
        let score = super::diffcore_rename::similarity_index(pair.score);
        rebuilt.push(ChangeDetached::Rewrite {
            source_location: source.path.clone(),
            source_entry_mode: mode_from_octal(source.mode),
            source_relation: None,
            source_id: source.oid,
            // The only consumer of this is the `similarity index` line, which is what
            // the score is carried in.
            diff: Some(gix::diff::blob::DiffLineStats {
                removals: 0,
                insertions: 0,
                before: 0,
                after: 0,
                similarity: score as f32 / 100.0,
            }),
            entry_mode: mode_from_octal(dest.mode),
            id: dest.oid,
            location: dest.path.clone(),
            relation: None,
            copy: status == b'C',
        });
    }
    rebuilt.sort_by(|a, b| change_path(a).cmp(change_path(b)));
    *changes = rebuilt;
    Ok(())
}

/// The `EntryMode` for a raw octal mode, as the queue carries it.
fn mode_from_octal(mode: u32) -> gix::objs::tree::EntryMode {
    gix::objs::tree::EntryMode::try_from(format!("{mode:o}").as_bytes())
        .unwrap_or(gix::objs::tree::EntryKind::Blob.into())
}

fn is_tree_entry(change: &ChangeDetached) -> bool {
    match change {
        ChangeDetached::Addition { entry_mode, .. }
        | ChangeDetached::Deletion { entry_mode, .. }
        | ChangeDetached::Modification { entry_mode, .. }
        | ChangeDetached::Rewrite { entry_mode, .. } => entry_mode.is_tree(),
    }
}

// ---------------------------------------------------------------------------
// Commit-message plumbing (pretty.c)
// ---------------------------------------------------------------------------

/// git's `get_one_line`: the length of the next line, newline included.
fn one_line(msg: &[u8]) -> usize {
    match msg.iter().position(|&b| b == b'\n') {
        Some(i) => i + 1,
        None => msg.len(),
    }
}

/// git's `is_blank_line`: right-trim the line and report whether nothing is left.
/// Returns the trimmed slice alongside the verdict.
fn blank_line(line: &[u8]) -> (&[u8], bool) {
    let t = trim_end_ws(line);
    (t, t.is_empty())
}

/// Strip trailing ASCII whitespace (git's `isspace` set).
fn trim_end_ws(mut s: &[u8]) -> &[u8] {
    while let Some(&last) = s.last() {
        if last.is_ascii_whitespace() {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

/// git's `skip_blank_lines`: advance past leading blank lines.
pub(super) fn skip_blank_lines(mut msg: &[u8]) -> &[u8] {
    loop {
        let len = one_line(msg);
        if len == 0 {
            return msg;
        }
        if !blank_line(&msg[..len]).1 {
            return msg;
        }
        msg = &msg[len..];
    }
}

/// git's `format_subject` with a `" "` separator: join the first paragraph into
/// one line and return it together with the rest of the message.
pub(super) fn format_subject(mut msg: &[u8]) -> (Vec<u8>, &[u8]) {
    let mut title: Vec<u8> = Vec::new();
    let mut first = true;
    loop {
        let len = one_line(msg);
        if len == 0 {
            break;
        }
        let (trimmed, is_blank) = blank_line(&msg[..len]);
        if is_blank {
            break;
        }
        msg = &msg[len..];
        if !first {
            title.push(b' ');
        }
        title.extend_from_slice(trimmed);
        first = false;
    }
    (title, msg)
}

/// git's `pp_remainder` with zero indent: skip leading blank lines, then emit
/// every remaining line right-trimmed.
pub(super) fn pp_remainder(msg: &[u8], out: &mut Vec<u8>) {
    pp_remainder_tabs(msg, out, 0);
}

/// The same with `pp->expand_tabs_in_log` in force (pretty.c:2281-2284): each
/// line is de-tabified to `tabwidth` columns instead of being copied through.
/// A `tabwidth` of zero is git's default and the plain path above.
fn pp_remainder_tabs(mut msg: &[u8], out: &mut Vec<u8>, tabwidth: usize) {
    let mut first = true;
    loop {
        let len = one_line(msg);
        if len == 0 {
            break;
        }
        let (trimmed, is_blank) = blank_line(&msg[..len]);
        msg = &msg[len..];
        if is_blank && first {
            continue;
        }
        first = false;
        if tabwidth == 0 {
            out.extend_from_slice(trimmed);
        } else {
            add_tabexpand(out, trimmed, tabwidth);
        }
        out.push(b'\n');
    }
}

/// Port of `strbuf_add_tabexpand()` (pretty.c:2183-2221): replace each tab with
/// the spaces that reach the next multiple of `tabwidth`, measuring what came
/// before it in *display columns*. A prefix that is not well-formed UTF-8 — or
/// whose width is undefined — makes git give up on aligning, so the rest of the
/// line is copied verbatim, tabs included.
fn add_tabexpand(out: &mut Vec<u8>, mut line: &[u8], tabwidth: usize) {
    while let Some(at) = line.iter().position(|&b| b == b'\t') {
        let Some(width) = pp_utf8_width(&line[..at]) else {
            break;
        };
        out.extend_from_slice(&line[..at]);
        out.resize(out.len() + tabwidth - (width % tabwidth), b' ');
        line = &line[at + 1..];
    }
    out.extend_from_slice(line);
}

/// `pp_utf8_width()` (pretty.c): the display width of `s`, or `None` when a byte
/// sequence is not well-formed UTF-8 or has no defined width.
fn pp_utf8_width(s: &[u8]) -> Option<usize> {
    let mut pos = 0usize;
    let mut width = 0i32;
    while pos < s.len() {
        let n = crate::utf8::utf8_width(s, &mut pos)?;
        if n < 0 {
            return None;
        }
        width += n;
    }
    Some(width as usize)
}

// ---------------------------------------------------------------------------
// Header encoding and wrapping (pretty.c, utf8.c)
// ---------------------------------------------------------------------------

/// Bytes already used on the last line of `sb` (git's `last_line_length`).
pub(super) fn last_line_length(sb: &str) -> i64 {
    match sb.rfind('\n') {
        Some(i) => (sb.len() - i - 1) as i64,
        None => sb.len() as i64,
    }
}

/// git's `needs_rfc2047_encoding`: any non-ASCII byte, a newline, or a literal
/// `=?` sequence forces the encoded-word form.
pub(super) fn needs_rfc2047_encoding(s: &str) -> bool {
    let b = s.as_bytes();
    for (i, &ch) in b.iter().enumerate() {
        if ch >= 0x80 || ch == b'\n' {
            return true;
        }
        if i + 1 < b.len() && ch == b'=' && b[i + 1] == b'?' {
            return true;
        }
    }
    false
}

/// git's `is_rfc822_special`.
fn is_rfc822_special(ch: u8) -> bool {
    matches!(
        ch,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b':' | b';' | b'@' | b',' | b'.' | b'"' | b'\\'
    )
}

/// git's `add_rfc822_quoted`: wrap in double quotes, backslash-escaping `"`/`\`.
fn rfc822_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// git's `is_rfc2047_special`. `address` selects the stricter `phrase` rules
/// used for the `From:` display name.
fn is_rfc2047_special(ch: u8, address: bool) -> bool {
    if ch >= 0x80 || !(0x20..0x7f).contains(&ch) {
        return true;
    }
    if ch.is_ascii_whitespace() || ch == b'=' || ch == b'?' || ch == b'_' {
        return true;
    }
    if !address {
        return false;
    }
    !(ch.is_ascii_alphanumeric() || matches!(ch, b'!' | b'*' | b'+' | b'-' | b'/'))
}

/// Port of `add_rfc2047()` (pretty.c): q-encoded words, never splitting a
/// multi-byte character, folded at 76 columns.
pub(super) fn add_rfc2047(sb: &mut String, line: &str, address: bool) {
    const MAX_ENCODED_LENGTH: i64 = 76;
    let mut line_len = last_line_length(sb);

    sb.push_str(&format!("=?{ENCODING}?q?"));
    line_len += ENCODING.len() as i64 + 5;

    for c in line.chars() {
        let mut buf = [0u8; 4];
        let bytes = c.encode_utf8(&mut buf).as_bytes();
        let chrlen = bytes.len() as i64;
        let is_special = chrlen > 1 || is_rfc2047_special(bytes[0], address);
        let encoded_len = if is_special { 3 * chrlen } else { 1 };

        if line_len + encoded_len + 2 > MAX_ENCODED_LENGTH {
            sb.push_str(&format!("?=\n =?{ENCODING}?q?"));
            line_len = ENCODING.len() as i64 + 5 + 1;
        }
        for &b in bytes {
            if is_special {
                sb.push_str(&format!("={b:02X}"));
            } else {
                sb.push(b as char);
            }
        }
        line_len += encoded_len;
    }
    sb.push_str("?=");
}

/// Port of `strbuf_add_wrapped_text()` (utf8.c) for the ASCII inputs that reach
/// it — anything non-ASCII takes the RFC2047 path above, and neither the subject
/// (paragraph joined with spaces) nor a display name can contain a newline, so
/// the original's embedded-newline branch is unreachable here.
///
/// A negative `indent1` means that many columns are already consumed.
pub(super) fn wrap_text(buf: &mut String, text: &str, indent1: i64, indent2: i64, width: i64) {
    if width <= 0 {
        buf.push_str(text);
        return;
    }
    let b = text.as_bytes();
    let mut indent = indent1;
    let mut w = indent1;
    let mut bol: usize = 0;
    let mut space: Option<usize> = None;
    let mut i: usize = 0;

    if indent < 0 {
        w = -indent;
        space = Some(0);
    }

    loop {
        let c = b.get(i).copied().unwrap_or(0);
        if c == 0 || c.is_ascii_whitespace() {
            if w <= width || space.is_none() {
                // git checks the empty-tail case against `bol`, before the
                // remembered space overrides the copy start.
                if c == 0 && i == bol {
                    return;
                }
                let start = match space {
                    Some(s) => s,
                    None => {
                        if indent > 0 {
                            buf.push_str(&" ".repeat(indent as usize));
                        }
                        bol
                    }
                };
                buf.push_str(&text[start..i]);
                if c == 0 {
                    return;
                }
                space = Some(i);
                if c == b'\t' {
                    w |= 0x07;
                }
                w += 1;
                i += 1;
            } else {
                // Break the line at the last remembered space.
                buf.push('\n');
                let s = space.expect("the else branch requires a remembered space");
                // `*space` reads the NUL terminator in git when the remembered
                // position is the end of the text; that is not whitespace.
                let at_space = b.get(s).copied().unwrap_or(0).is_ascii_whitespace();
                i = s + usize::from(at_space);
                bol = i;
                space = None;
                indent = indent2;
                w = indent2;
            }
            continue;
        }
        w += 1;
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// Diffstat and summary (diff.c)
// ---------------------------------------------------------------------------

/// One diffstat row: the quoted path and its line counts. `raw_name` is the
/// unquoted path, which is what git's `dirstat` groups on.
struct StatEntry {
    name: String,
    raw_name: Vec<u8>,
    added: u64,
    deleted: u64,
    /// `diffstat_file.comments`: `get_compact_summary()`'s word for this pair,
    /// present only under `--compact-summary`. It joins `print_name`, which the
    /// `--stat` row uses, and never the raw name `--numstat` prints.
    comment: Option<&'static str>,
    /// `diffstat_file.is_binary`, with the two sizes `show_stats()` prints as
    /// `Bin <old> -> <new> bytes`. A binary pair contributes no line counts, which is
    /// why `--numstat` prints `-` for both and the shortstat counts the file alone.
    binary: Option<(u64, u64)>,
}

/// The four `show_stats()` width knobs, as this verb records them.
///
/// format-patch is the one caller that never reaches git's `-1` sentinels:
/// `cmd_format_patch()` does its own `repo_init_revisions()` + `setup_revisions()`
/// rather than going through `cmd_log_init()`, so `init_diffstat_widths()` never
/// runs and every field starts at 0 (`builtin/log.c:2102`, `:2216`). That is why
/// `if (!rev.diffopt.stat_width) rev.diffopt.stat_width = MAIL_DEFAULT_WRAP;`
/// (`builtin/log.c:2233`) actually fires here and `term_columns()` never does.
fn stat_widths(o: &Opts) -> StatWidths {
    StatWidths {
        width: o.stat_width,
        name_width: o.stat_name_width,
        graph_width: o.stat_graph_width,
        count: o.stat_count,
    }
}



/// The rows [`super::diffstat::show_stats`] renders. A binary pair becomes the `Bin <old>
/// -> <new> bytes` row `show_stats()` prints for one; no row is ever unmerged.
fn stat_rows(files: &[StatEntry]) -> Vec<diffstat::StatFile> {
    files
        .iter()
        .map(|f| {
            // `fill_print_name()`: the quoted name, then the unquoted
            // ` (<comment>)` annotation.
            let mut print_name = f.name.clone().into_bytes();
            if let Some(comment) = f.comment {
                print_name.extend_from_slice(format!(" ({comment})").as_bytes());
            }
            match f.binary {
                Some((old_size, new_size)) => diffstat::StatFile {
                    print_name,
                    added: new_size,
                    deleted: old_size,
                    binary: true,
                    is_unmerged: false,
                },
                None => diffstat::StatFile::text(print_name, f.added, f.deleted),
            }
        })
        .collect()
}

/// `show_stats()` (diff.c) at format-patch's width: `MAIL_DEFAULT_WRAP` (72)
/// unless `--stat`/`--stat-width` named one, never the terminal width.
fn emit_stats(
    out: &mut Vec<u8>,
    files: &[StatEntry],
    sw: StatWidths,
    colors: &DiffColors,
) -> Result<()> {
    let sw = StatWidths {
        width: if sw.width != 0 { sw.width } else { MAIL_DEFAULT_WRAP },
        ..sw
    };
    diffstat::show_stats(out, &stat_rows(files), &sw, colors);
    Ok(())
}
/// `show_shortstats()` (diff.c) — the trailing ` N files changed, …` line, which
/// is also the whole of `--shortstat`.
fn emit_stat_summary(out: &mut Vec<u8>, files: &[StatEntry]) -> Result<()> {
    diffstat::show_shortstats(out, &stat_rows(files));
    Ok(())
}
/// Port of `show_numstat()` (diff.c): tab-separated counts and the C-quoted path.
fn emit_numstat(out: &mut Vec<u8>, files: &[StatEntry]) -> Result<()> {
    for f in files {
        // `show_numstat()`: a binary pair prints `-` for both counts, since there are no
        // lines to count.
        match f.binary {
            Some(_) => writeln!(out, "-\t-\t{}", f.name)?,
            None => writeln!(out, "{}\t{}\t{}", f.added, f.deleted, f.name)?,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Dirstat (diff.c, diffcore-delta.c)
// ---------------------------------------------------------------------------

/// One entry of git's `struct dirstat_dir`: a path and its damage.
struct DirstatFile {
    name: Vec<u8>,
    changed: u64,
}

/// Port of `show_dirstat()` (diff.c). Damage is measured in bytes of the
/// pre-image that did not survive plus bytes that are new, except in
/// `--dirstat-by-file` mode where every changed file counts as exactly one.
fn emit_dirstat(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    changes: &[ChangeDetached],
    cfg: &Dirstat,
) -> Result<()> {
    let mut files: Vec<DirstatFile> = Vec::new();
    let mut changed: u64 = 0;

    for change in changes {
        // `p->one->oid_valid && p->two->oid_valid && oideq(&p->one->oid,
        // &p->two->oid)`: an unchanged blob id means identical content, whatever
        // else moved. git tests that *before* the `--dirstat-by-file` shortcut,
        // so a pure rename contributes nothing to either mode — verified against
        // stock git 2.55.0, whose `format-patch --dirstat-by-file` prints no
        // dirstat block at all for a commit that only renames a file.
        //
        // An addition or a deletion has only one valid side, so the test cannot
        // fire for it and its damage is its whole size (or 1 by file).
        let unchanged_blob = match change {
            ChangeDetached::Modification {
                previous_id, id, ..
            } => previous_id == id,
            ChangeDetached::Rewrite { source_id, id, .. } => source_id == id,
            _ => false,
        };
        let damage = match change {
            _ if unchanged_blob => 0,
            _ if cfg.by_file => 1,
            ChangeDetached::Modification {
                previous_entry_mode,
                previous_id,
                entry_mode,
                id,
                ..
            } => {
                let old = content_of(repo, *previous_id, previous_entry_mode.is_commit())?;
                let new = content_of(repo, *id, entry_mode.is_commit())?;
                let (copied, added) = count_changes(&old, &new);
                // Original minus copied is the removed material; `added` is the
                // new material. Both are damage done to the pre-image, and a
                // changed id always means at least one unit of it.
                ((old.len() as u64 - copied) + added).max(1)
            }
            ChangeDetached::Deletion {
                entry_mode, id, ..
            } => content_of(repo, *id, entry_mode.is_commit())?.len() as u64,
            ChangeDetached::Addition {
                entry_mode, id, ..
            } => content_of(repo, *id, entry_mode.is_commit())?.len() as u64,
            // A rename is charged the damage between its two sides, the same way a
            // modification is; an unchanged move was already handled above.
            ChangeDetached::Rewrite {
                source_entry_mode,
                source_id,
                entry_mode,
                id,
                ..
            } => {
                let old = content_of(repo, *source_id, source_entry_mode.is_commit())?;
                let new = content_of(repo, *id, entry_mode.is_commit())?;
                let (added, copied) = count_changes(&old, &new);
                ((old.len() as u64 - copied) + added).max(1)
            }
        };
        // `found_damage: if (!damage) continue;` — a file that did no damage never
        // joins `dir->files`, and `gather_dirstat()`'s `sources` counter can tell
        // the difference between an absent entry and a zero-valued one.
        if damage == 0 {
            continue;
        }
        files.push(DirstatFile {
            name: change_path(change).to_vec(),
            changed: damage,
        });
        changed += damage;
    }

    conclude_dirstat(out, files, changed, cfg)
}

/// Port of `show_dirstat_by_line()` (diff.c): the same report, with damage taken
/// from the diffstat's line counts instead of from the blob contents.
fn emit_dirstat_by_line(out: &mut Vec<u8>, stats: &[StatEntry], cfg: &Dirstat) -> Result<()> {
    if stats.is_empty() {
        return Ok(());
    }
    let mut changed: u64 = 0;
    let files: Vec<DirstatFile> = stats
        .iter()
        .map(|f| {
            let damage = f.added + f.deleted;
            changed += damage;
            DirstatFile {
                name: f.raw_name.clone(),
                changed: damage,
            }
        })
        .collect();
    conclude_dirstat(out, files, changed, cfg)
}

/// Port of `conclude_dirstat()` (diff.c): sort by path, then walk.
fn conclude_dirstat(
    out: &mut Vec<u8>,
    mut files: Vec<DirstatFile>,
    changed: u64,
    cfg: &Dirstat,
) -> Result<()> {
    if changed == 0 {
        return Ok(());
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    let mut cursor = 0usize;
    gather_dirstat(out, &files, &mut cursor, changed, 0, cfg)?;
    Ok(())
}

/// Port of `gather_dirstat()` (diff.c).
///
/// `cursor` is git's consuming `dir->files++`/`dir->nr--`: the recursion walks
/// the sorted list once, and each level reports the directory named by the first
/// `baselen` bytes of the entry that opened it. A directory is silent at the top
/// level and whenever everything under it came from a single subdirectory
/// (`sources == 1`), which is what keeps the report to the branch points.
fn gather_dirstat(
    out: &mut Vec<u8>,
    files: &[DirstatFile],
    cursor: &mut usize,
    changed: u64,
    baselen: usize,
    cfg: &Dirstat,
) -> Result<u64> {
    let mut sum_changes: u64 = 0;
    let mut sources = 0u32;
    // The base is a prefix of the entry that opened this level; borrowing it
    // across the recursion would alias `files`, so it is captured up front.
    let base: Vec<u8> = files
        .get(*cursor)
        .map(|f| f.name[..baselen.min(f.name.len())].to_vec())
        .unwrap_or_default();

    while *cursor < files.len() {
        let name = &files[*cursor].name;
        if name.len() < baselen || name[..baselen] != base[..] {
            break;
        }
        let changes = match name[baselen..].iter().position(|&b| b == b'/') {
            Some(slash) => {
                let newbaselen = baselen + slash + 1;
                sources += 1;
                gather_dirstat(out, files, cursor, changed, newbaselen, cfg)?
            }
            None => {
                let changes = files[*cursor].changed;
                *cursor += 1;
                sources += 2;
                changes
            }
        };
        sum_changes += changes;
    }

    if baselen > 0 && sources != 1 && sum_changes > 0 {
        let permille = sum_changes * 1000 / changed;
        if permille >= u64::from(cfg.permille) {
            write!(out, "{:4}.{}% ", permille / 10, permille % 10)?;
            out.extend_from_slice(&base);
            out.push(b'\n');
            if !cfg.cumulative {
                return Ok(0);
            }
        }
    }
    Ok(sum_changes)
}

/// Port of `diffcore_count_changes()` (diffcore-delta.c): returns
/// `(src_copied, literal_added)` for the byte-level dirstat.
///
/// Both buffers are cut into chunks that end at an LF or after 64 bytes,
/// whichever comes first, and the chunks are hashed into counting buckets. A
/// chunk the destination has at least as many of as the source was copied; the
/// surplus on either side is what changed.
fn count_changes(src: &[u8], dst: &[u8]) -> (u64, u64) {
    let src_count = hash_chars(src);
    let dst_count = hash_chars(dst);

    let (mut sc, mut la) = (0u64, 0u64);
    let (mut s, mut d) = (0usize, 0usize);
    while s < src_count.len() && src_count[s].1 != 0 {
        while d < dst_count.len() && dst_count[d].1 != 0 {
            if dst_count[d].0 >= src_count[s].0 {
                break;
            }
            la += u64::from(dst_count[d].1);
            d += 1;
        }
        let src_cnt = src_count[s].1;
        let mut dst_cnt = 0u32;
        if d < dst_count.len() && dst_count[d].1 != 0 && dst_count[d].0 == src_count[s].0 {
            dst_cnt = dst_count[d].1;
            d += 1;
        }
        if src_cnt < dst_cnt {
            la += u64::from(dst_cnt - src_cnt);
            sc += u64::from(src_cnt);
        } else {
            sc += u64::from(dst_cnt);
        }
        s += 1;
    }
    while d < dst_count.len() && dst_count[d].1 != 0 {
        la += u64::from(dst_count[d].1);
        d += 1;
    }
    (sc, la)
}

/// git's `HASHBASE`: a prime chosen so the table never has to grow past 2^18.
const HASHBASE: u32 = 107_927;

/// git's `INITIAL_HASH_SIZE`.
const INITIAL_HASH_LOG2: u32 = 9;

/// git's `INITIAL_FREE`: leave proportionally more slack in a small table.
fn initial_free(log2: u32) -> i64 {
    i64::from((1u32 << log2) * (log2 - 3) / log2)
}

/// Port of `hash_chars()` (diffcore-delta.c): the chunked rolling hash, returned
/// as git leaves it — a power-of-two table sorted so that live buckets come
/// first, in hash order, and empty ones sort to the end.
fn hash_chars(buf: &[u8]) -> Vec<(u32, u32)> {
    // `is_text` only controls CRLF folding; binary content never reaches here.
    let mut table: Vec<(u32, u32)> = vec![(0, 0); 1 << INITIAL_HASH_LOG2];
    let mut log2 = INITIAL_HASH_LOG2;
    let mut free = initial_free(log2);

    let (mut accum1, mut accum2) = (0u32, 0u32);
    let mut n = 0u32;
    let mut i = 0usize;
    while i < buf.len() {
        let c = buf[i];
        i += 1;
        // Ignore CR in a CRLF sequence.
        if c == b'\r' && buf.get(i) == Some(&b'\n') {
            continue;
        }
        let old_1 = accum1;
        accum1 = (accum1 << 7) ^ (accum2 >> 25);
        accum2 = (accum2 << 7) ^ (old_1 >> 25);
        accum1 = accum1.wrapping_add(u32::from(c));
        n += 1;
        if n < 64 && c != b'\n' {
            continue;
        }
        let hashval = accum1.wrapping_add(accum2.wrapping_mul(0x61)) % HASHBASE;
        add_spanhash(&mut table, &mut log2, &mut free, hashval, n);
        n = 0;
        accum1 = 0;
        accum2 = 0;
    }
    if n > 0 {
        let hashval = accum1.wrapping_add(accum2.wrapping_mul(0x61)) % HASHBASE;
        add_spanhash(&mut table, &mut log2, &mut free, hashval, n);
    }

    // git's `spanhash_cmp`: empty buckets last, live ones by hash value.
    table.sort_by(|a, b| match (a.1 == 0, b.1 == 0) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => a.0.cmp(&b.0),
    });
    table
}

/// Port of `add_spanhash()` + `spanhash_rehash()` (diffcore-delta.c): linear
/// probing, and a doubling rehash once the free budget is spent.
fn add_spanhash(table: &mut Vec<(u32, u32)>, log2: &mut u32, free: &mut i64, hashval: u32, cnt: u32) {
    let lim = 1usize << *log2;
    let mut bucket = (hashval as usize) & (lim - 1);
    loop {
        let slot = &mut table[bucket];
        bucket += 1;
        if slot.1 == 0 {
            *slot = (hashval, cnt);
            *free -= 1;
            if *free < 0 {
                spanhash_rehash(table, log2, free);
            }
            return;
        }
        if slot.0 == hashval {
            slot.1 += cnt;
            return;
        }
        if lim <= bucket {
            bucket = 0;
        }
    }
}

fn spanhash_rehash(table: &mut Vec<(u32, u32)>, log2: &mut u32, free: &mut i64) {
    let sz = 1usize << (*log2 + 1);
    let mut grown: Vec<(u32, u32)> = vec![(0, 0); sz];
    *log2 += 1;
    *free = initial_free(*log2);
    for &(hashval, cnt) in table.iter() {
        if cnt == 0 {
            continue;
        }
        let mut bucket = (hashval as usize) & (sz - 1);
        loop {
            let slot = &mut grown[bucket];
            bucket += 1;
            if slot.1 == 0 {
                *slot = (hashval, cnt);
                *free -= 1;
                break;
            }
            if sz <= bucket {
                bucket = 0;
            }
        }
    }
    *table = grown;
}

/// Port of `diff_flush_raw()` (diff.c:6469-6503): the `:<mode> <mode> <sha>
/// <sha> <status><TAB><path>` line the `--raw` family prints.
///
/// The object names use the same abbreviation the `index` lines do
/// (`diff_aligned_abbrev(&oid, opt->abbrev)`), an absent side is all zeros at the
/// same width, and a score — a rename/copy similarity or `-B`'s dissimilarity —
/// is appended to the status letter as three zero-padded digits (`R100`, `M100`).
fn emit_raw(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    changes: &[ChangeDetached],
    abbrev: Abbrev,
    dissimilarity: &HashMap<Vec<u8>, u32>,
    opts: &Opts,
) -> Result<()> {
    for change in changes {
        let (old, new, status, score, names): (_, _, u8, Option<u32>, Vec<&[u8]>) = match change {
            ChangeDetached::Addition {
                location,
                entry_mode,
                id,
                ..
            } => (None, Some((*id, entry_mode)), b'A', None, vec![location]),
            ChangeDetached::Deletion {
                location,
                entry_mode,
                id,
                ..
            } => (Some((*id, entry_mode)), None, b'D', None, vec![location]),
            ChangeDetached::Modification {
                location,
                previous_entry_mode,
                previous_id,
                entry_mode,
                id,
            } => {
                // `DIFF_PAIR_TYPE_CHANGED()`: the file type bits differ, so a
                // regular file became a symlink or a gitlink (or the reverse).
                let changed_type = (u32::from(previous_entry_mode.value())
                    ^ u32::from(entry_mode.value()))
                    & 0o170000
                    != 0;
                (
                    Some((*previous_id, previous_entry_mode)),
                    Some((*id, entry_mode)),
                    if changed_type { b'T' } else { b'M' },
                    dissimilarity.get(location.as_slice()).copied(),
                    vec![location],
                )
            }
            ChangeDetached::Rewrite {
                source_location,
                source_entry_mode,
                source_id,
                location,
                entry_mode,
                id,
                diff,
                copy,
                ..
            } => (
                Some((*source_id, source_entry_mode)),
                Some((*id, entry_mode)),
                if *copy { b'C' } else { b'R' },
                Some(similarity_percent(diff.as_ref())),
                vec![source_location, location],
            ),
        };
        let width = |side: &Option<(ObjectId, &gix::objs::tree::EntryMode)>| -> Result<usize> {
            match side {
                Some((id, mode)) => Ok(short_oid(repo, *id, abbrev, mode.is_commit())?.len()),
                None => Ok(0),
            }
        };
        // An absent side has no object to disambiguate, so it borrows the other
        // side's width — which is what `diff_abbrev_oid()` on a null oid produces.
        let hex_len = width(&old)?.max(width(&new)?);
        let name = |side: &Option<(ObjectId, &gix::objs::tree::EntryMode)>| -> Result<String> {
            Ok(match side {
                Some((id, mode)) => short_oid(repo, *id, abbrev, mode.is_commit())?,
                None => "0".repeat(hex_len),
            })
        };
        write!(
            out,
            ":{:06o} {:06o} {} {} {}",
            old.map_or(0, |(_, m)| u32::from(m.value())),
            new.map_or(0, |(_, m)| u32::from(m.value())),
            name(&old)?,
            name(&new)?,
            status as char,
        )?;
        if let Some(score) = score {
            write!(out, "{score:03}")?;
        }
        // `diff_flush_raw()` reports each name through `strip_prefix()`.
        for path in names {
            out.push(b'\t');
            out.extend_from_slice(quote_path(strip_relative(path, opts)).as_bytes());
        }
        out.push(b'\n');
    }
    Ok(())
}

/// Port of `is_summary_empty()` (diff.c): whether `--summary` would print
/// nothing, which decides whether it counts toward `diff_flush()`'s separator.
fn is_summary_empty(changes: &[ChangeDetached], dissimilarity: &HashMap<Vec<u8>, u32>) -> bool {
    !changes.iter().any(|c| match c {
        ChangeDetached::Addition { .. }
        | ChangeDetached::Deletion { .. }
        | ChangeDetached::Rewrite { .. } => true,
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            entry_mode,
            ..
        } => {
            // `if (p->score) return 0` (diff.c:7017): `-B`'s ` rewrite <path> (<n>%)`
            // is a summary line of its own, with or without a mode change.
            dissimilarity.contains_key(location.as_slice())
                || previous_entry_mode.value() != entry_mode.value()
        }
    })
}

/// Port of `diff_summary()` (diff.c:6803-6831): the `create`/`delete`/`rename`/
/// `copy`/`rewrite`/`mode change` lines that follow the diffstat.
fn emit_summary(
    out: &mut Vec<u8>,
    changes: &[ChangeDetached],
    dissimilarity: &HashMap<Vec<u8>, u32>,
) -> Result<()> {
    for change in changes {
        match change {
            ChangeDetached::Addition {
                location,
                entry_mode,
                ..
            } => writeln!(
                out,
                " create mode {:06o} {}",
                entry_mode.value(),
                quote_path(location)
            )?,
            ChangeDetached::Deletion {
                location,
                entry_mode,
                ..
            } => writeln!(
                out,
                " delete mode {:06o} {}",
                entry_mode.value(),
                quote_path(location)
            )?,
            ChangeDetached::Modification {
                location,
                previous_entry_mode,
                entry_mode,
                ..
            } => {
                // A pair `-B` broke and `diffcore_merge_broken()` glued back
                // together carries a score, which `diff_summary()` prints as its
                // own ` rewrite` line — and which then suppresses the *name* on
                // the mode-change line (`show_mode_change(opt, p, !p->score)`).
                let score = dissimilarity.get(location.as_slice());
                if let Some(score) = score {
                    writeln!(out, " rewrite {} ({score}%)", quote_path(location))?;
                }
                if previous_entry_mode.value() != entry_mode.value() {
                    write!(
                        out,
                        " mode change {:06o} => {:06o}",
                        previous_entry_mode.value(),
                        entry_mode.value(),
                    )?;
                    if score.is_none() {
                        write!(out, " {}", quote_path(location))?;
                    }
                    out.push(b'\n');
                }
            }
            // `diff_summary()`'s rename/copy line, with the similarity in percent.
            ChangeDetached::Rewrite {
                source_location,
                location,
                diff,
                copy,
                ..
            } => writeln!(
                out,
                " {} {} ({}%)",
                if *copy { "copy" } else { "rename" },
                // `show_rename_copy()` prints the compacted `pkg/{a.txt => b.txt}`
                // form, the same one the diffstat row uses.
                quote_path(&super::diff_pairs::pprint_rename(source_location, location)),
                similarity_percent(diff.as_ref())
            )?,
        }
    }
    Ok(())
}

/// The `similarity index` percentage carried on a rewrite.
fn similarity_percent(diff: Option<&gix::diff::blob::DiffLineStats>) -> u32 {
    diff.map_or(100, |d| (d.similarity * 100.0).round() as u32)
}

// ---------------------------------------------------------------------------
// Patch body (shared shape with `show`)
// ---------------------------------------------------------------------------

/// Render one file-level change as a `diff --git` block, returning its stat row.
fn emit_change(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    change: &ChangeDetached,
    abbrev: Abbrev,
    opts: &Opts,
    dissimilarity: &HashMap<Vec<u8>, u32>,
) -> Result<(StatEntry, FilePaint)> {
    let mut counts = (0u64, 0u64);
    // `diffstat_file.is_binary` plus the two sizes the `Bin <n> -> <m> bytes` row prints.
    let mut binary: Option<(u64, u64)> = None;
    // ```c
    // if (o->flags.binary) {
    //         mmfile_t mf;
    //         if ((!fill_mmfile(o->repo, &mf, one) && diff_filespec_is_binary(o->repo, one)) ||
    //             (!fill_mmfile(o->repo, &mf, two) && diff_filespec_is_binary(o->repo, two)))
    //                 abbrev = hexsz;
    // }
    // ```
    //
    // (`fill_metainfo()`, diff.c.) A binary pair in a `--binary` diff names both blobs in
    // full: the payload can only be applied against the exact pre-image, so the `index`
    // line has to identify it unambiguously.
    let hexsz = repo.object_hash().len_in_hex();
    let index_abbrev = |binary: bool| match binary && !opts.no_binary {
        true => Abbrev::Fixed(hexsz),
        false => abbrev,
    };
    // `ecbdata.blank_at_eof_in_preimage` / `_postimage`, left at zero for the pairs
    // that never reach a textual diff (a pure mode change, a rename that moved the
    // content untouched, `-D`'s discarded deletion body).
    let mut blank_at_eof = (0usize, 0usize);
    match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            let path: &[u8] = location;
            emit_git_header(out, path, opts);
            writeln!(out, "new file mode {:o}", entry_mode.value())?;
            let is_sub = entry_mode.is_commit();
            let content = content_of(repo, *id, is_sub)?;
            binary = pair_is_binary(is_sub, &content, opts).then(|| (0, content.len() as u64));
            let short = short_oid(repo, *id, index_abbrev(binary.is_some()), is_sub)?;
            writeln!(out, "index {}..{}", "0".repeat(short.len()), short)?;
            counts = emit_body(
                repo, out, None, Some(path), &[], &content, opts, binary.is_some(), &mut blank_at_eof,
            )?;
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            let path: &[u8] = location;
            emit_git_header(out, path, opts);
            writeln!(out, "deleted file mode {:o}", entry_mode.value())?;
            let is_sub = entry_mode.is_commit();
            let content = content_of(repo, *id, is_sub)?;
            binary = pair_is_binary(is_sub, &content, opts).then(|| (content.len() as u64, 0));
            let short = short_oid(repo, *id, index_abbrev(binary.is_some()), is_sub)?;
            writeln!(out, "index {}..{}", short, "0".repeat(short.len()))?;
            // `-D`/`--irreversible-delete`: `builtin_diff()` stops as soon as it
            // sees `/dev/null` on the post-image side, so a deletion carries no
            // body. The diffstat is computed by its own pass and is untouched,
            // so the removed lines are still counted here.
            let mut sink = Vec::new();
            let body = if opts.irreversible_delete { &mut sink } else { &mut *out };
            counts = emit_body(
                repo, body, Some(path), None, &content, &[], opts, binary.is_some(), &mut blank_at_eof,
            )?;
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            let path: &[u8] = location;
            emit_git_header(out, path, opts);
            let old_mode = format!("{:o}", previous_entry_mode.value());
            let new_mode = format!("{:o}", entry_mode.value());
            let mode_changed = old_mode != new_mode;
            if mode_changed {
                writeln!(out, "old mode {old_mode}")?;
                writeln!(out, "new mode {new_mode}")?;
            }
            // `-B` left a score on this pair: `fill_metainfo()` prints it as the
            // `dissimilarity index` that stands where a rename's `similarity
            // index` would (diff.c:4897-4903).
            if let Some(score) = dissimilarity.get(path) {
                writeln!(out, "dissimilarity index {score}%")?;
            }
            // A pure mode change (identical content) prints no index/hunks.
            if previous_id != id {
                let old_is_sub = previous_entry_mode.is_commit();
                let new_is_sub = entry_mode.is_commit();
                let old_content = content_of(repo, *previous_id, old_is_sub)?;
                let new_content = content_of(repo, *id, new_is_sub)?;
                binary = (pair_is_binary(old_is_sub, &old_content, opts)
                    || pair_is_binary(new_is_sub, &new_content, opts))
                .then(|| (old_content.len() as u64, new_content.len() as u64));
                let old_short =
                    short_oid(repo, *previous_id, index_abbrev(binary.is_some()), old_is_sub)?;
                let new_short = short_oid(repo, *id, index_abbrev(binary.is_some()), new_is_sub)?;
                // The mode suffix is dropped when `old mode`/`new mode` said it.
                if mode_changed {
                    writeln!(out, "index {old_short}..{new_short}")?;
                } else {
                    writeln!(out, "index {old_short}..{new_short} {new_mode}")?;
                }
                counts = emit_body(
                    repo,
                    out,
                    Some(path),
                    Some(path),
                    &old_content,
                    &new_content,
                    opts,
                    binary.is_some(),
                    &mut blank_at_eof,
                )?;
            }
        }
        ChangeDetached::Rewrite {
            source_location,
            source_entry_mode,
            source_id,
            entry_mode,
            id,
            location,
            diff,
            copy,
            ..
        } => {
            let (from, to): (&[u8], &[u8]) = (source_location, location);
            emit_rename_header(out, from, to, opts);
            writeln!(out, "similarity index {}%", similarity_percent(diff.as_ref()))?;
            let verb = if *copy { "copy" } else { "rename" };
            // `fill_metainfo()` names both sides through `p->one->path` /
            // `p->two->path` after `run_diff()` has already applied
            // `strip_prefix()` (diff.c:5009), so `--relative` shortens these two
            // lines exactly as it shortens the `diff --git` header above them.
            writeln!(out, "{verb} from {}", quote_path(strip_relative(from, opts)))?;
            writeln!(out, "{verb} to {}", quote_path(strip_relative(to, opts)))?;
            let old_mode = format!("{:o}", source_entry_mode.value());
            let new_mode = format!("{:o}", entry_mode.value());
            if old_mode != new_mode {
                writeln!(out, "old mode {old_mode}")?;
                writeln!(out, "new mode {new_mode}")?;
            }
            // A rename that moved the content unchanged has nothing below the header.
            if source_id != id {
                let old_is_sub = source_entry_mode.is_commit();
                let new_is_sub = entry_mode.is_commit();
                let old_content = content_of(repo, *source_id, old_is_sub)?;
                let new_content = content_of(repo, *id, new_is_sub)?;
                binary = (pair_is_binary(old_is_sub, &old_content, opts)
                    || pair_is_binary(new_is_sub, &new_content, opts))
                .then(|| (old_content.len() as u64, new_content.len() as u64));
                let old_short =
                    short_oid(repo, *source_id, index_abbrev(binary.is_some()), old_is_sub)?;
                let new_short = short_oid(repo, *id, index_abbrev(binary.is_some()), new_is_sub)?;
                if old_mode != new_mode {
                    writeln!(out, "index {old_short}..{new_short}")?;
                } else {
                    writeln!(out, "index {old_short}..{new_short} {new_mode}")?;
                }
                counts = emit_body(
                    repo,
                    out,
                    Some(from),
                    Some(to),
                    &old_content,
                    &new_content,
                    opts,
                    binary.is_some(),
                    &mut blank_at_eof,
                )?;
            }
        }
    }
    // The stat row for a rename names both sides through `pprint_rename()`.
    // `run_diffstat()` calls `strip_prefix()` first, so `--relative` shortens the
    // two names it is built from.
    let display = match change {
        ChangeDetached::Rewrite {
            source_location,
            location,
            ..
        } => super::diff_pairs::pprint_rename(
            strip_relative(source_location, opts),
            strip_relative(location, opts),
        ),
        _ => strip_relative(change_path(change), opts).to_vec(),
    };
    // `diffstat_add()` (diff.c:2856) keeps the two apart: `x->name` is the
    // destination path and `x->from_name` the source, and only `show_stats()` joins
    // them through `pprint_rename()`. `show_dirstat_by_line()` groups on `x->name`,
    // so a rename must not carry the `{a => b}` form into the dirstat.
    let raw_name = strip_relative(change_dest_path(change), opts).to_vec();
    // `get_compact_summary()` (diff.c:4156-4180), reached only under
    // `--compact-summary`; a rename/copy skips the `new`/`gone` words because both
    // of its sides exist, which is exactly what the two modes already say.
    let comment = opts.compact_summary.then(|| {
        let (old_mode, new_mode) = change_modes(change);
        super::diff::compact_comment_for_modes(old_mode, new_mode)
    });
    Ok((
        StatEntry {
            name: quote_path(&display),
            raw_name,
            added: counts.0,
            deleted: counts.1,
            comment: comment.flatten(),
            binary,
        },
        FilePaint { ws_rule: opts.ws_rule, blank_at_eof },
    ))
}

/// `p->two->path`: the path the change ends at, which is the destination of a
/// rename or copy rather than [`change_path`]'s source-side name.
fn change_dest_path(change: &ChangeDetached) -> &[u8] {
    match change {
        ChangeDetached::Rewrite { location, .. } => location,
        _ => change_path(change),
    }
}

/// The two mode words a change's file pair carries, `None` on the side the file
/// does not exist.
fn change_modes(change: &ChangeDetached) -> (Option<u32>, Option<u32>) {
    match change {
        ChangeDetached::Addition { entry_mode, .. } => (None, Some(u32::from(entry_mode.value()))),
        ChangeDetached::Deletion { entry_mode, .. } => (Some(u32::from(entry_mode.value())), None),
        ChangeDetached::Modification {
            previous_entry_mode,
            entry_mode,
            ..
        } => (
            Some(u32::from(previous_entry_mode.value())),
            Some(u32::from(entry_mode.value())),
        ),
        ChangeDetached::Rewrite {
            source_entry_mode,
            entry_mode,
            ..
        } => (
            Some(u32::from(source_entry_mode.value())),
            Some(u32::from(entry_mode.value())),
        ),
    }
}

/// `diff --git a/<old> b/<new>` for a rename, where the two paths differ.
fn emit_rename_header(out: &mut Vec<u8>, from: &[u8], to: &[u8], opts: &Opts) {
    let (from, to) = (strip_relative(from, opts), strip_relative(to, opts));
    let (a, b) = prefixes(opts);
    out.extend_from_slice(b"diff --git ");
    out.extend_from_slice(&quote_two(a, from));
    out.push(b' ');
    out.extend_from_slice(&quote_two(b, to));
    out.push(b'\n');
}

/// `diff_filespec_is_binary()`: a NUL in the first 8000 bytes makes a blob binary, unless
/// `-a`/`--text` asked for it to be treated as text. A gitlink is never binary — its
/// "content" is the synthesized `Subproject commit …` line.
fn pair_is_binary(is_submodule: bool, content: &[u8], opts: &Opts) -> bool {
    !opts.text && !is_submodule && is_binary(content)
}

/// `diff --git a/<path> b/<path>` line, with git's `quote_two()` C-quoting.
/// Under `--no-prefix`/`format.noprefix` both prefixes are empty, so git emits
/// `diff --git <path> <path>`.
fn emit_git_header(out: &mut Vec<u8>, path: &[u8], opts: &Opts) {
    let path = strip_relative(path, opts);
    let (a, b) = prefixes(opts);
    out.extend_from_slice(b"diff --git ");
    out.extend_from_slice(&quote_two(a, path));
    out.push(b' ');
    out.extend_from_slice(&quote_two(b, path));
    out.push(b'\n');
}

/// `strip_prefix()` (diff.c:5009), `--relative`'s *shortening* half. Only
/// `run_diff()`, `run_diffstat()`, `run_checkdiff()`, `diff_flush_raw()` and
/// `flush_one_pair()` call it — `diff_summary()` and `show_dirstat()` do not, which
/// is why `--relative=src --summary` still prints `src/new/moved.txt` (measured
/// against stock 2.55.0).
fn strip_relative<'a>(path: &'a [u8], opts: &Opts) -> &'a [u8] {
    let Some(prefix) = opts.active_relative() else {
        return path;
    };
    if path.len() < prefix.len() {
        return path;
    }
    // `*namep += prefix_length; if (**namep == '/') ++*namep;` — the byte count comes
    // off first and only *then* is a leading separator eaten, which is why
    // `--relative=src` strips four characters off `src/f` while `--relative=sr`
    // strips two and leaves `c/f`.
    let rest = &path[prefix.len()..];
    match rest.first() {
        Some(b'/') => &rest[1..],
        _ => rest,
    }
}

/// The `a/`+`b/` source/destination prefixes, emptied by `--no-prefix` /
/// `format.noprefix`.
fn prefixes(opts: &Opts) -> (&str, &str) {
    if opts.noprefix {
        return ("", "");
    }
    (opts.src_prefix.as_str(), opts.dst_prefix.as_str())
}

/// Emit the `---`/`+++` headers and hunks, returning `(added, deleted)` line
/// counts. An add/delete of an empty file produces no header lines, like git.
#[allow(clippy::too_many_arguments)]
fn emit_body(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
    old_content: &[u8],
    new_content: &[u8],
    opts: &Opts,
    binary: bool,
    blank_at_eof: &mut (usize, usize),
) -> Result<(u64, u64)> {
    // ```c
    // if (o->flags.binary)
    //         emit_binary_diff(o, &mf1, &mf2);
    // else {
    //         [...]
    //         emit_diff_symbol(o, DIFF_SYMBOL_BINARY_FILES, sb.buf, sb.len, 0);
    // }
    // ```
    //
    // (`builtin_diff()`, diff.c.) `format-patch` implies `--binary`, so a binary pair
    // renders as the base85 payload; `--no-binary` leaves the one-line notice instead.
    // Neither carries a `---`/`+++` pair or any hunk, and neither counts lines.
    if binary {
        if !opts.no_binary {
            super::binary_patch::emit(
                out,
                old_content,
                new_content,
                super::binary_patch::loose_compression_level(repo),
            );
        } else {
            let (a, b) = prefixes(opts);
            let label = |side: Option<&[u8]>, prefix: &str| match side {
                Some(p) => quote_two(prefix, strip_relative(p, opts)),
                None => b"/dev/null".to_vec(),
            };
            out.extend_from_slice(b"Binary files ");
            out.extend_from_slice(&label(old, a));
            out.extend_from_slice(b" and ");
            out.extend_from_slice(&label(new, b));
            out.extend_from_slice(b" differ\n");
        }
        return Ok((0, 0));
    }
    // `builtin_diff()` runs `check_blank_at_eof()` over the two whole images before
    // the hunks are produced (diff.c:4048-4049); the `WS_BLANK_AT_EOF` gate lives
    // downstream in `ws_check_emit()`, so the counts are taken unconditionally here
    // exactly as `git diff`'s own path takes them (diff_pairs.rs:5430).
    *blank_at_eof = diff_color::check_blank_at_eof(old_content, new_content);
    let mut hunks: Vec<u8> = Vec::new();
    let counts = emit_text_hunks(&mut hunks, old_content, new_content, opts)?;
    if hunks.is_empty() {
        return Ok(counts);
    }

    let (a, b) = prefixes(opts);
    emit_file_header(out, b"--- ", old.map(|p| strip_relative(p, opts)), a);
    emit_file_header(out, b"+++ ", new.map(|p| strip_relative(p, opts)), b);
    out.extend_from_slice(&hunks);
    Ok(counts)
}

/// One `---`/`+++` line. git appends a tab when the rendered name contains a
/// space, so that a reader can tell where the name ends.
fn emit_file_header(out: &mut Vec<u8>, marker: &[u8], path: Option<&[u8]>, prefix: &str) {
    out.extend_from_slice(marker);
    let name = match path {
        Some(p) => quote_two(prefix, p),
        None => b"/dev/null".to_vec(),
    };
    out.extend_from_slice(&name);
    if name.contains(&b' ') {
        out.push(b'\t');
    }
    out.push(b'\n');
}

/// Compute the unified diff of two blobs, returning the added/deleted line
/// counts the diffstat needs.
fn emit_text_hunks(
    out: &mut Vec<u8>,
    old: &[u8],
    new: &[u8],
    opts: &Opts,
) -> Result<(u64, u64)> {
    // `-w`/`-b`/`--ignore-space-at-eol`/`--ignore-cr-at-eol`: `xdl_recmatch()`
    // compares canonicalised records while the emitter prints the originals, so
    // the interner is fed normalized lines and every emitted line is indexed out
    // of the raw ones. The hand-rolled `xdl_emit_diff()` port is the only emitter
    // that can keep the two apart, so the whitespace flags route through it.
    if opts.ws != super::diff_pairs::Whitespace::Keep {
        let before = super::diff_pairs::byte_lines(old);
        let after = super::diff_pairs::byte_lines(new);
        let mut input: InternedInput<Vec<u8>> = InternedInput::default();
        input.update_before(
            before
                .iter()
                .map(|l| super::diff_pairs::normalize(l, opts.ws)),
        );
        input.update_after(
            after
                .iter()
                .map(|l| super::diff_pairs::normalize(l, opts.ws)),
        );
        // `xdl_change_compact()`'s `get_indent()` reads the unmodified record, so
        // the slider heuristic scores the raw lines, not the normalized tokens.
        let diff =
            super::diff_pairs::compute_compacted(opts.algorithm, &input, &before, &after, opts.indent_heuristic);
        return emit_hunks_with_ignorable(out, &diff, &before, &after, opts);
    }

    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(opts.algorithm, &input);
    let before_lines: Vec<&[u8]> = input.before.iter().map(|&t| input.interner[t]).collect();

    // gix's `UnifiedDiff` emits xdiff's default hunk shape only; `-I`,
    // `--ignore-blank-lines`, `-W` and `--inter-hunk-context` each change which
    // change groups share a hunk and how far its context reaches, so they go
    // through the hand-rolled `xdl_emit_diff()` port.
    if !opts.ignore_regex.is_empty()
        || opts.ignore_blank_lines
        || opts.function_context
        || opts.inter_hunk_ctx != 0
    {
        let after_lines: Vec<&[u8]> = input.after.iter().map(|&t| input.interner[t]).collect();
        return emit_hunks_with_ignorable(out, &diff, &before_lines, &after_lines, opts);
    }

    let writer = HunkWriter {
        out,
        before_lines,
        indicators: opts.indicators,
        added: 0,
        deleted: 0,
    };
    let counts = UnifiedDiff::new(
        &diff,
        &input,
        writer,
        ContextSize::symmetrical(opts.context),
    )
    .consume()?;
    Ok(counts)
}

/// One entry of xdiff's edit script (`struct xdchange`).
struct Change {
    i1: u32,
    chg1: u32,
    i2: u32,
    chg2: u32,
    ignore: bool,
}

/// Port of `xdl_emit_diff()` (xdiff/xemit.c), used whenever the hunk geometry is
/// not xdiff's default: `-I`, `--ignore-blank-lines`, `-W` or
/// `--inter-hunk-context`.
///
/// gix's `UnifiedDiff` groups every change into a hunk, which is right until a
/// change can be *ignorable*: git marks those in `xdl_mark_ignorable_lines()` /
/// `xdl_mark_ignorable_regex()` and then lets `xdl_get_hunk()` drop the ones that
/// no real change is holding in place. An ignorable change close to a real one is
/// still printed — they suppress hunks, not lines — so the two must be decided
/// together.
fn emit_hunks_with_ignorable(
    out: &mut Vec<u8>,
    diff: &gix::diff::blob::Diff,
    before_lines: &[&[u8]],
    after_lines: &[&[u8]],
    opts: &Opts,
) -> Result<(u64, u64)> {
    // `xdl_blankline()` (xdiff/xutils.c:142-153): with no `XDF_WHITESPACE_FLAGS` bit
    // set a record is blank only when it is empty or a bare terminator; once any of
    // them is on, a record of nothing but whitespace counts.
    let ws_on = opts.ws != super::diff_pairs::Whitespace::Keep;
    let blank = |line: &[u8]| -> bool {
        if ws_on {
            line.iter().all(|b| b.is_ascii_whitespace())
        } else {
            line.len() <= 1
        }
    };
    // Both markers start from `ignore = 1` and walk the pre-image then the
    // post-image, so an empty side simply does not object.
    let all = |lines: &[&[u8]], pred: &dyn Fn(&[u8]) -> bool| lines.iter().all(|l| pred(l));
    let changes: Vec<Change> = diff
        .hunks()
        .map(|h| {
            let (b1, b2) = (h.before.start as usize, h.before.end as usize);
            let (a1, a2) = (h.after.start as usize, h.after.end as usize);
            // `xdl_mark_ignorable_lines()` runs first, and 2.55.0's
            // `xdl_mark_ignorable_regex()` opens with `if (xch->ignore) continue;`
            // (xdiff/xdiffi.c:1070-1073) — the blank-line verdict is never
            // overridden, so the two are an or, not a last-writer-wins.
            let blank_ignorable = opts.ignore_blank_lines
                && all(&before_lines[b1..b2], &blank)
                && all(&after_lines[a1..a2], &blank);
            let regex_ignorable = !opts.ignore_regex.is_empty() && {
                let m = |line: &[u8]| opts.ignore_regex.iter().any(|re| re.is_match(line));
                all(&before_lines[b1..b2], &m) && all(&after_lines[a1..a2], &m)
            };
            Change {
                i1: h.before.start,
                chg1: h.before.end - h.before.start,
                i2: h.after.start,
                chg2: h.after.end - h.after.start,
                ignore: blank_ignorable || regex_ignorable,
            }
        })
        .collect();

    let ctx = i64::from(opts.context);
    let nrec1 = before_lines.len() as i64;
    let nrec2 = after_lines.len() as i64;

    let mut writer = HunkWriter {
        out,
        before_lines: before_lines.to_vec(),
        indicators: opts.indicators,
        added: 0,
        deleted: 0,
    };

    let func_context = opts.function_context;
    let mut idx = 0usize;
    while idx < changes.len() {
        // `xchp` in xdiff: the first change of the group *before* `xdl_get_hunk`
        // skipped any leading ignorable ones. Growing the leading context back
        // over one of those brings it into the hunk after all.
        let xchp = idx;
        let mut start = idx;
        let Some(last) = get_hunk(&changes, &mut start, ctx, opts.inter_hunk_ctx) else {
            break;
        };
        let (mut first, mut last) = (start, last);

        // `pre_context_calculation`, re-entered whenever the widened context
        // swallowed a change that had been left out.
        let (mut s1, mut s2);
        loop {
            let f = &changes[first];
            s1 = (i64::from(f.i1) - ctx).max(0);
            s2 = (i64::from(f.i2) - ctx).max(0);
            if !func_context {
                break;
            }
            let mut i1 = i64::from(f.i1);
            // An appended chunk has nothing above it in the pre-image; if the
            // whole function came with it, no extra context is needed at all.
            let mut appended_whole_function = false;
            if i1 >= nrec1 {
                let mut i2 = i64::from(f.i2);
                while i2 < nrec2 {
                    if is_func_line(after_lines[i2 as usize]) {
                        appended_whole_function = true;
                        break;
                    }
                    i2 += 1;
                }
                i1 = nrec1 - 1;
            }
            if appended_whole_function {
                break;
            }
            let mut fs1 = get_func_line(before_lines, i1, -1);
            while fs1 > 0
                && !is_empty_line(before_lines[(fs1 - 1) as usize])
                && !is_func_line(before_lines[(fs1 - 1) as usize])
            {
                fs1 -= 1;
            }
            if fs1 < 0 {
                fs1 = 0;
            }
            if fs1 >= s1 {
                break;
            }
            s2 = (s2 - (s1 - fs1)).max(0);
            s1 = fs1;
            // Did the widened context reach back over a skipped change?
            let mut back = xchp;
            while back != first
                && i64::from(changes[back].i1 + changes[back].chg1) <= s1
                && i64::from(changes[back].i2 + changes[back].chg2) <= s2
            {
                back += 1;
            }
            if back == first {
                break;
            }
            first = back;
        }

        // `post_context_calculation`, re-entered whenever the trailing context
        // ran into the next change.
        let (mut e1, mut e2);
        loop {
            let e = &changes[last];
            // Trailing context stops at whichever file runs out first.
            let lctx = ctx
                .min(nrec1 - i64::from(e.i1 + e.chg1))
                .min(nrec2 - i64::from(e.i2 + e.chg2));
            e1 = i64::from(e.i1 + e.chg1) + lctx;
            e2 = i64::from(e.i2 + e.chg2) + lctx;
            if !func_context {
                break;
            }
            let mut fe1 = get_func_line(before_lines, i64::from(e.i1 + e.chg1), nrec1);
            while fe1 > 0 && is_empty_line(before_lines[(fe1 - 1) as usize]) {
                fe1 -= 1;
            }
            if fe1 < 0 {
                fe1 = nrec1;
            }
            if fe1 > e1 {
                e2 = (e2 + (fe1 - e1)).min(nrec2);
                e1 = fe1;
            }
            // Overlap with the next change? Then take it in and start over.
            let Some(next) = changes.get(last + 1) else {
                break;
            };
            let l = i64::from(next.i1).min(nrec1 - 1);
            if l - ctx <= e1 || get_func_line(before_lines, l, e1) < 0 {
                last += 1;
                continue;
            }
            break;
        }

        let (f, e) = (&changes[first], &changes[last]);
        let mut lines: Vec<(DiffLineKind, &[u8])> = Vec::new();
        // Leading context, taken from the post-image as xdiff does.
        for l in s2..i64::from(f.i2) {
            lines.push((DiffLineKind::Context, after_lines[l as usize]));
        }
        let (mut c1, mut c2) = (i64::from(f.i1), i64::from(f.i2));
        for ch in &changes[first..=last] {
            // Context bridging this change and the previous one in the hunk.
            while c1 < i64::from(ch.i1) && c2 < i64::from(ch.i2) {
                lines.push((DiffLineKind::Context, after_lines[c2 as usize]));
                c1 += 1;
                c2 += 1;
            }
            for l in ch.i1..ch.i1 + ch.chg1 {
                lines.push((DiffLineKind::Remove, before_lines[l as usize]));
            }
            for l in ch.i2..ch.i2 + ch.chg2 {
                lines.push((DiffLineKind::Add, after_lines[l as usize]));
            }
            c1 = i64::from(ch.i1 + ch.chg1);
            c2 = i64::from(ch.i2 + ch.chg2);
        }
        for l in i64::from(e.i2 + e.chg2)..e2 {
            lines.push((DiffLineKind::Context, after_lines[l as usize]));
        }

        let header = HunkHeader {
            before_hunk_start: (s1 + 1) as u32,
            before_hunk_len: (e1 - s1) as u32,
            after_hunk_start: (s2 + 1) as u32,
            after_hunk_len: (e2 - s2) as u32,
        };
        writer.consume_hunk(header, &lines)?;

        idx = last + 1;
    }

    Ok(writer.finish())
}

/// Port of `def_ff()` (xdiff/xemit.c), the funcname heuristic git uses when no
/// `xfuncname` driver applies: a line whose first byte starts an identifier.
fn is_func_line(line: &[u8]) -> bool {
    matches!(line.first(), Some(&c) if c.is_ascii_alphabetic() || c == b'_' || c == b'$')
}

/// Port of `is_empty_rec()` (xdiff/xemit.c): a record of nothing but whitespace.
/// Records carry their newline, so a blank line is empty by this test.
fn is_empty_line(line: &[u8]) -> bool {
    line.iter()
        .all(|&c| matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'))
}

/// Port of `get_func_line()` (xdiff/xemit.c): scan the *pre-image* from `start`
/// toward `limit` (exclusive) for the first line [`is_func_line`] accepts, or
/// `-1` when there is none. The direction follows the sign of `limit - start`,
/// which is how the same routine finds both the function a hunk sits inside and
/// the one that starts after it.
fn get_func_line(lines: &[&[u8]], start: i64, limit: i64) -> i64 {
    let step: i64 = if start > limit { -1 } else { 1 };
    let n = lines.len() as i64;
    let mut l = start;
    while l != limit && l >= 0 && l < n {
        if is_func_line(lines[l as usize]) {
            return l;
        }
        l += step;
    }
    -1
}

/// Port of `xdl_get_hunk()` (xdiff/xemit.c). `max_common` is
/// `ctxlen + ctxlen + interhunkctxlen` (xemit.c:58-60), so `--inter-hunk-context`
/// widens the gap two change groups may leave and still share one hunk.
///
/// Advances `start` past leading ignorable changes that no following change is
/// close enough to rescue, then returns the index of the last change that
/// belongs in the same hunk — or `None` once nothing is left to show.
fn get_hunk(
    changes: &[Change],
    start: &mut usize,
    ctxlen: i64,
    interhunkctxlen: i64,
) -> Option<usize> {
    let max_common = ctxlen + ctxlen + interhunkctxlen;
    let max_ignorable = ctxlen;
    let end_of = |i: usize| i64::from(changes[i].i1 + changes[i].chg1);

    let mut p = *start;
    while p < changes.len() && changes[p].ignore {
        let next = p + 1;
        if next >= changes.len() || i64::from(changes[next].i1) - end_of(p) >= max_ignorable {
            *start = next;
        }
        p = next;
    }
    if *start >= changes.len() {
        return None;
    }

    let mut ignored: i64 = 0;
    let mut last = *start;
    let mut prev = *start;
    let mut cur = *start + 1;
    while cur < changes.len() {
        let distance = i64::from(changes[cur].i1) - end_of(prev);
        if distance > max_common {
            break;
        }
        if distance < max_ignorable && (!changes[cur].ignore || last == prev) {
            last = cur;
            ignored = 0;
        } else if distance < max_ignorable && changes[cur].ignore {
            ignored += i64::from(changes[cur].chg2);
        } else if last != prev && i64::from(changes[cur].i1) + ignored - end_of(last) > max_common {
            break;
        } else if !changes[cur].ignore {
            last = cur;
            ignored = 0;
        } else {
            ignored += i64::from(changes[cur].chg2);
        }
        prev = cur;
        cur += 1;
    }
    Some(last)
}

/// Writes hunks in git's unified-diff style and tallies changed lines.
struct HunkWriter<'a> {
    out: &'a mut Vec<u8>,
    /// Pre-image lines, for resolving each hunk header's function context.
    before_lines: Vec<&'a [u8]>,
    /// `--output-indicator-new/-old/-context`, as `(new, old, context)`.
    indicators: (u8, u8, u8),
    added: u64,
    deleted: u64,
}

impl<'a> HunkWriter<'a> {
    /// Nearest "function" line above the hunk's leading context, mirroring git's
    /// default (no `xfuncname`) heuristic: first byte is a letter, `_`, or `$`.
    fn find_func(&self, before_hunk_start: u32) -> Option<&'a [u8]> {
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

impl ConsumeHunk for HunkWriter<'_> {
    type Out = (u64, u64);

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
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
                DiffLineKind::Context => self.indicators.2,
                DiffLineKind::Add => {
                    self.added += 1;
                    self.indicators.0
                }
                DiffLineKind::Remove => {
                    self.deleted += 1;
                    self.indicators.1
                }
            });
            self.out.extend_from_slice(content);
            if !content.ends_with(b"\n") {
                self.out.push(b'\n');
                self.out
                    .extend_from_slice(b"\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) -> (u64, u64) {
        (self.added, self.deleted)
    }
}

/// Port of `xdl_emit_hunk_hdr()` (xdiff): the `,len` field is omitted when the
/// hunk spans exactly one line, and an empty side is anchored to the line
/// *before* the change — which is line 0 for a file that is being created.
fn write_range(out: &mut Vec<u8>, start: u32, len: u32) {
    let start = if len == 0 { start.saturating_sub(1) } else { start };
    if len == 1 {
        let _ = write!(out, "{start}");
    } else {
        let _ = write!(out, "{start},{len}");
    }
}

/// The bytes to diff for an entry: a blob comes from the object database; a
/// submodule (commit entry) renders as its `Subproject commit <oid>` line.
fn content_of(repo: &gix::Repository, id: ObjectId, is_submodule: bool) -> Result<Vec<u8>> {
    if is_submodule {
        Ok(format!("Subproject commit {}\n", id.to_hex()).into_bytes())
    } else {
        Ok(repo.find_object(id)?.detach().data)
    }
}

/// Abbreviated object id for the `index` line. Real objects are disambiguated
/// against the odb; a submodule commit (absent here) is plainly truncated.
fn short_oid(
    repo: &gix::Repository,
    id: ObjectId,
    abbrev: Abbrev,
    is_submodule: bool,
) -> Result<String> {
    let hexsz = repo.object_hash().len_in_hex();
    let min = match abbrev {
        // `fill_metainfo()` reads `abbrev = o->abbrev ? o->abbrev : DEFAULT_ABBREV`
        // (diff.c:4915), so an unset `--abbrev` (and `--no-abbrev`, which stores 0)
        // both land on the automatic length `core.abbrev` picks.
        Abbrev::Auto(auto) => {
            return Ok(if is_submodule {
                id.to_hex_with_len(auto).to_string()
            } else {
                id.attach(repo).shorten()?.to_string()
            })
        }
        Abbrev::Fixed(n) => n,
    };
    // `--full-index` raises `abbrev` to `the_hash_algo->hexsz` in
    // `diff_setup_done()`, and `diff_abbrev_oid()` then prints the whole name
    // without asking the object store how short it could be. A submodule commit is
    // absent from this object store, so it can only be truncated.
    if is_submodule || min >= hexsz {
        return Ok(id.to_hex_with_len(min).to_string());
    }
    // `repo_find_unique_abbrev(oid, min)`: start at the requested width and grow
    // one hex digit at a time until the prefix names exactly one object.
    let candidate = gix::odb::store::prefix::disambiguate::Candidate::new(id, min)?;
    Ok(match repo.objects.disambiguate_prefix(candidate)? {
        Some(prefix) => prefix.to_string(),
        None => id.to_hex_with_len(min).to_string(),
    })
}

/// `diff_options.abbrev` after `diff_setup_done()`, in the two shapes that behave
/// differently: the automatic per-object length, and a floor a caller asked for.
#[derive(Clone, Copy)]
enum Abbrev {
    /// `DEFAULT_ABBREV` — `core.abbrev`, carrying the length a submodule commit
    /// (which this object store cannot disambiguate) is truncated to.
    Auto(usize),
    /// `--abbrev=<n>` or `--full-index`: a minimum width, extended only as far as
    /// uniqueness demands.
    Fixed(usize),
}

/// The abbreviation every `index` line of one tree diff uses: git's
/// `diff_setup_done()` pins it to the full hash length under `--full-index`.
fn index_abbrev(repo: &gix::Repository, tree: &gix::Tree<'_>, opts: &Opts) -> Result<Abbrev> {
    if opts.full_index {
        return Ok(Abbrev::Fixed(repo.object_hash().len_in_hex()));
    }
    if let Some(n) = opts.abbrev {
        return Ok(Abbrev::Fixed(n));
    }
    Ok(Abbrev::Auto(tree.id().shorten()?.hex_len()))
}

/// The abbreviation the `--raw` block uses. `diff_flush_raw()` reads
/// `opt->abbrev` straight (diff.c:6477-6479) while `fill_metainfo()` lets
/// `--full-index` override it (diff.c:4917), so `--raw --full-index` prints seven
/// hex digits in the raw line and forty in the `index` line.
fn raw_abbrev(repo: &gix::Repository, tree: &gix::Tree<'_>, opts: &Opts) -> Result<Abbrev> {
    match opts.abbrev {
        Some(n) => Ok(Abbrev::Fixed(n)),
        None => Ok(Abbrev::Auto(tree.id().shorten()?.hex_len())),
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

/// `quote_c_style()`: the name verbatim unless some byte needs escaping, in which
/// case the whole name double-quoted with C escapes. The table and the
/// `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    crate::quote::quoted_name_string(path.as_ref())
}

/// git's `quote_two()`: `<prefix><path>` is quoted as a whole when either half
/// needs escaping, so `a/` stays inside the quotes.
fn quote_two(prefix: &str, path: &[u8]) -> Vec<u8> {
    crate::quote::quote_two_c_style(prefix.as_bytes(), path)
}

// ---------------------------------------------------------------------------
// POSIX extended regular expressions (`regcomp(REG_EXTENDED | REG_NEWLINE)`)
// ---------------------------------------------------------------------------
//
// git compiles each `-I<regex>` with `REG_EXTENDED | REG_NEWLINE` and asks only
// whether the record matches anywhere, so this engine answers a boolean and
// keeps no capture state. `REG_NEWLINE` is the part that surprises: `^` also
// matches immediately after a newline and `$` immediately before one, so on a
// record like `deep\n` the position past the newline satisfies both at once and
// the pattern `^$` matches every line that ends in a newline — which is exactly
// what stock git does with `-I^$`.
//
// The program is a Thompson NFA run as a parallel state set, so a pattern like
// `(a*)*b` costs O(len × program) instead of backtracking exponentially.

/// A parsed regular expression, before it is flattened into a program.
enum Node {
    Empty,
    /// One character drawn from a set: a literal, `.`, or a bracket expression.
    Set(CharSet),
    /// `^` — start of buffer, or just after a newline.
    Bol,
    /// `$` — end of buffer, or just before a newline.
    Eol,
    Cat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: u32,
        /// `None` is an unbounded tail, as in `*`, `+` and `{n,}`.
        max: Option<u32>,
    },
}

/// A bracket expression, `.`, or a single literal.
#[derive(Clone)]
struct CharSet {
    negated: bool,
    ranges: Vec<(char, char)>,
    classes: Vec<Class>,
}

/// The POSIX character classes usable as `[:name:]`.
#[derive(Clone, Copy, PartialEq)]
enum Class {
    Alnum,
    Alpha,
    Blank,
    Cntrl,
    Digit,
    Graph,
    Lower,
    Print,
    Punct,
    Space,
    Upper,
    Xdigit,
}

impl Class {
    fn parse(name: &str) -> Option<Class> {
        Some(match name {
            "alnum" => Class::Alnum,
            "alpha" => Class::Alpha,
            "blank" => Class::Blank,
            "cntrl" => Class::Cntrl,
            "digit" => Class::Digit,
            "graph" => Class::Graph,
            "lower" => Class::Lower,
            "print" => Class::Print,
            "punct" => Class::Punct,
            "space" => Class::Space,
            "upper" => Class::Upper,
            "xdigit" => Class::Xdigit,
            _ => return None,
        })
    }

    fn matches(self, c: char) -> bool {
        match self {
            Class::Alnum => c.is_alphanumeric(),
            Class::Alpha => c.is_alphabetic(),
            Class::Blank => c == ' ' || c == '\t',
            Class::Cntrl => c.is_control(),
            Class::Digit => c.is_ascii_digit(),
            Class::Graph => !c.is_whitespace() && !c.is_control(),
            Class::Lower => c.is_lowercase(),
            Class::Print => !c.is_control(),
            Class::Punct => c.is_ascii_punctuation(),
            Class::Space => c.is_whitespace(),
            Class::Upper => c.is_uppercase(),
            Class::Xdigit => c.is_ascii_hexdigit(),
        }
    }
}

impl CharSet {
    /// A single literal character.
    fn literal(c: char) -> CharSet {
        CharSet {
            negated: false,
            ranges: vec![(c, c)],
            classes: Vec::new(),
        }
    }

    /// `.` — under `REG_NEWLINE` this is "anything but a newline", which is what
    /// an empty negated set already means.
    fn any() -> CharSet {
        CharSet {
            negated: true,
            ranges: Vec::new(),
            classes: Vec::new(),
        }
    }

    fn matches(&self, c: char) -> bool {
        let listed = self.ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi)
            || self.classes.iter().any(|cl| cl.matches(c));
        if self.negated {
            // REG_NEWLINE: a non-matching list never matches a newline.
            !listed && c != '\n'
        } else {
            listed
        }
    }
}

/// One instruction of the compiled NFA. Every instruction that does not branch
/// falls through to the next one.
enum Inst {
    Char(CharSet),
    Split(usize, usize),
    Jump(usize),
    Bol,
    Eol,
    Match,
}

/// A compiled regular expression.
struct Regex {
    prog: Vec<Inst>,
}

/// Anything `regcomp` would reject. git only reports that it happened, never
/// which rule was broken, so the reason is not carried.
struct RegexError;

impl Regex {
    fn compile(pattern: &str) -> std::result::Result<Regex, RegexError> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut parser = Parser { chars, pos: 0 };
        let node = parser.parse_alt()?;
        if parser.pos != parser.chars.len() {
            // A `)` with no `(` is all that can be left over.
            return Err(RegexError);
        }
        let mut prog = Vec::new();
        emit_node(&node, &mut prog);
        prog.push(Inst::Match);
        Ok(Regex { prog })
    }

    /// Whether the pattern matches anywhere in `text` — git's `regexec_buf()`
    /// call passes the whole record, trailing newline included, and only looks
    /// at the return code.
    fn is_match(&self, text: &[u8]) -> bool {
        let chars = decode_chars(text);
        let n = chars.len();

        let mut current: Vec<usize> = Vec::new();
        let mut next: Vec<usize> = Vec::new();
        let mut seen = vec![usize::MAX; self.prog.len()];

        for pos in 0..=n {
            // A fresh thread at every position is what makes the search
            // unanchored, as `regexec` without `REG_STARTEND` anchoring is.
            let mut stack = std::mem::take(&mut current);
            stack.push(0);

            while let Some(pc) = stack.pop() {
                if seen[pc] == pos {
                    continue;
                }
                seen[pc] = pos;
                match &self.prog[pc] {
                    Inst::Jump(t) => stack.push(*t),
                    Inst::Split(a, b) => {
                        stack.push(*a);
                        stack.push(*b);
                    }
                    Inst::Bol => {
                        if pos == 0 || chars[pos - 1] == '\n' {
                            stack.push(pc + 1);
                        }
                    }
                    Inst::Eol => {
                        if pos == n || chars[pos] == '\n' {
                            stack.push(pc + 1);
                        }
                    }
                    Inst::Match => return true,
                    Inst::Char(set) => {
                        if pos < n && set.matches(chars[pos]) {
                            next.push(pc + 1);
                        }
                    }
                }
            }
            current = std::mem::take(&mut next);
        }
        false
    }
}

/// Decode `text` into characters. Well-formed UTF-8 decodes as itself; any byte
/// that does not start a valid sequence stands for the character of the same
/// value, so no input is ever rejected.
fn decode_chars(text: &[u8]) -> Vec<char> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let width = match text[i] {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 0,
        };
        let decoded = (width > 1 && i + width <= text.len())
            .then(|| std::str::from_utf8(&text[i..i + width]).ok())
            .flatten()
            .and_then(|s| s.chars().next());
        match decoded {
            Some(c) => {
                out.push(c);
                i += width;
            }
            None => {
                out.push(char::from(text[i]));
                i += 1;
            }
        }
    }
    out
}

/// Flatten the parse tree into the NFA program.
fn emit_node(node: &Node, prog: &mut Vec<Inst>) {
    match node {
        Node::Empty => {}
        Node::Set(set) => prog.push(Inst::Char(set.clone())),
        Node::Bol => prog.push(Inst::Bol),
        Node::Eol => prog.push(Inst::Eol),
        Node::Cat(parts) => {
            for part in parts {
                emit_node(part, prog);
            }
        }
        Node::Alt(branches) => {
            // Each branch gets a split that either enters it or moves on, and
            // ends with a jump to the common exit, patched once all are placed.
            let mut jumps = Vec::new();
            for (i, branch) in branches.iter().enumerate() {
                if i + 1 == branches.len() {
                    emit_node(branch, prog);
                    break;
                }
                let split = prog.len();
                prog.push(Inst::Split(0, 0));
                emit_node(branch, prog);
                jumps.push(prog.len());
                prog.push(Inst::Jump(0));
                let next = prog.len();
                prog[split] = Inst::Split(split + 1, next);
            }
            let exit = prog.len();
            for j in jumps {
                prog[j] = Inst::Jump(exit);
            }
        }
        Node::Repeat { node, min, max } => {
            for _ in 0..*min {
                emit_node(node, prog);
            }
            match max {
                None => {
                    // `X*`: split into the body or past it, and loop back.
                    let split = prog.len();
                    prog.push(Inst::Split(0, 0));
                    emit_node(node, prog);
                    prog.push(Inst::Jump(split));
                    let exit = prog.len();
                    prog[split] = Inst::Split(split + 1, exit);
                }
                Some(max) => {
                    // `X{n,m}`: the surplus copies are each independently optional.
                    let mut splits = Vec::new();
                    for _ in *min..*max {
                        splits.push(prog.len());
                        prog.push(Inst::Split(0, 0));
                        emit_node(node, prog);
                    }
                    let exit = prog.len();
                    for s in splits {
                        prog[s] = Inst::Split(s + 1, exit);
                    }
                }
            }
        }
    }
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    /// `alt := concat ('|' concat)*`
    fn parse_alt(&mut self) -> std::result::Result<Node, RegexError> {
        let mut branches = vec![self.parse_concat()?];
        while self.peek() == Some('|') {
            self.pos += 1;
            branches.push(self.parse_concat()?);
        }
        Ok(if branches.len() == 1 {
            branches.pop().expect("just checked the length")
        } else {
            Node::Alt(branches)
        })
    }

    /// `concat := repeat*`, stopping at `|` or the `)` that closes a group.
    fn parse_concat(&mut self) -> std::result::Result<Node, RegexError> {
        let mut parts = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            parts.push(self.parse_repeat()?);
        }
        Ok(match parts.len() {
            0 => Node::Empty,
            1 => parts.pop().expect("just checked the length"),
            _ => Node::Cat(parts),
        })
    }

    /// `repeat := atom ('*' | '+' | '?' | '{n,m}')*`
    fn parse_repeat(&mut self) -> std::result::Result<Node, RegexError> {
        let mut node = self.parse_atom()?;
        loop {
            let (min, max) = match self.peek() {
                Some('*') => (0, None),
                Some('+') => (1, None),
                Some('?') => (0, Some(1)),
                Some('{') => match self.parse_interval()? {
                    Some(bounds) => {
                        node = Node::Repeat {
                            node: Box::new(node),
                            min: bounds.0,
                            max: bounds.1,
                        };
                        continue;
                    }
                    // Not a valid interval, so `{` was an ordinary character and
                    // `parse_interval` left the position untouched.
                    None => break,
                },
                _ => break,
            };
            self.pos += 1;
            node = Node::Repeat {
                node: Box::new(node),
                min,
                max,
            };
        }
        Ok(node)
    }

    /// `{n}`, `{n,}` or `{n,m}`. Returns `None` — without consuming anything —
    /// when what follows is not an interval, which leaves `{` an ordinary
    /// character as the C libraries treat it.
    fn parse_interval(&mut self) -> std::result::Result<Option<(u32, Option<u32>)>, RegexError> {
        let save = self.pos;
        self.pos += 1;
        let Some(min) = self.parse_bound() else {
            self.pos = save;
            return Ok(None);
        };
        let max = match self.peek() {
            Some('}') => Some(min),
            Some(',') => {
                self.pos += 1;
                if self.peek() == Some('}') {
                    None
                } else {
                    match self.parse_bound() {
                        Some(max) => Some(max),
                        None => {
                            self.pos = save;
                            return Ok(None);
                        }
                    }
                }
            }
            _ => {
                self.pos = save;
                return Ok(None);
            }
        };
        if self.peek() != Some('}') {
            self.pos = save;
            return Ok(None);
        }
        self.pos += 1;
        if max.is_some_and(|max| max < min) {
            return Err(RegexError);
        }
        Ok(Some((min, max)))
    }

    fn parse_bound(&mut self) -> Option<u32> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if start == self.pos {
            return None;
        }
        self.chars[start..self.pos]
            .iter()
            .collect::<String>()
            .parse()
            .ok()
    }

    fn parse_atom(&mut self) -> std::result::Result<Node, RegexError> {
        let Some(c) = self.peek() else {
            return Err(RegexError);
        };
        self.pos += 1;
        Ok(match c {
            '^' => Node::Bol,
            '$' => Node::Eol,
            '.' => Node::Set(CharSet::any()),
            '(' => {
                let inner = self.parse_alt()?;
                if self.peek() != Some(')') {
                    return Err(RegexError);
                }
                self.pos += 1;
                inner
            }
            '[' => Node::Set(self.parse_bracket()?),
            // A repetition operator with nothing to repeat is `REG_BADRPT`.
            '*' | '+' | '?' => return Err(RegexError),
            ')' => return Err(RegexError),
            '\\' => match self.peek() {
                Some(esc) => {
                    self.pos += 1;
                    Node::Set(CharSet::literal(esc))
                }
                None => return Err(RegexError),
            },
            other => Node::Set(CharSet::literal(other)),
        })
    }

    /// A bracket expression. POSIX gives backslash no special meaning in here,
    /// `]` first is a literal, and `-` first or last is a literal.
    fn parse_bracket(&mut self) -> std::result::Result<CharSet, RegexError> {
        let mut set = CharSet {
            negated: false,
            ranges: Vec::new(),
            classes: Vec::new(),
        };
        if self.peek() == Some('^') {
            set.negated = true;
            self.pos += 1;
        }
        let mut first = true;
        loop {
            let Some(c) = self.peek() else {
                // Unterminated: this is what rejects `-I'['`.
                return Err(RegexError);
            };
            if c == ']' && !first {
                self.pos += 1;
                return Ok(set);
            }
            first = false;

            if c == '[' && self.chars.get(self.pos + 1) == Some(&':') {
                let rest: String = self.chars[self.pos + 2..].iter().collect();
                let Some(end) = rest.find(":]") else {
                    return Err(RegexError);
                };
                let Some(class) = Class::parse(&rest[..end]) else {
                    return Err(RegexError);
                };
                set.classes.push(class);
                self.pos += 2 + rest[..end].chars().count() + 2;
                continue;
            }

            self.pos += 1;
            // `a-z`, unless the `-` is the last character before `]`.
            if self.peek() == Some('-') && self.chars.get(self.pos + 1).is_some_and(|&n| n != ']') {
                let Some(&hi) = self.chars.get(self.pos + 1) else {
                    return Err(RegexError);
                };
                if hi < c {
                    return Err(RegexError);
                }
                set.ranges.push((c, hi));
                self.pos += 2;
            } else {
                set.ranges.push((c, c));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, added: u64, deleted: u64) -> StatEntry {
        StatEntry {
            name: name.to_owned(),
            raw_name: name.as_bytes().to_vec(),
            added,
            deleted,
            comment: None,
            // A text pair: the counts above are the whole story, and
            // `binary` is what tells `--numstat` to print `-` instead.
            binary: None,
        }
    }

    fn stats(files: &[StatEntry], sw: StatWidths) -> String {
        let mut out = Vec::new();
        emit_stats(&mut out, files, sw, &DiffColors::disabled()).expect("emit_stats writes to a Vec");
        String::from_utf8(out).expect("diffstat output is UTF-8")
    }

    /// Port of `diff_opt_stat()`'s `--stat=<w>[,<nw>[,<c>]]` field parse: `width`
    /// is always taken (an empty value is 0), and each later field is `Some` only
    /// when its comma is reached, so an absent field keeps the previous value.
    #[test]
    fn stat_value_fields() {
        assert_eq!(parse_stat_value(b"20,10,3"), Some((20, Some(10), Some(3))));
        assert_eq!(parse_stat_value(b"50"), Some((50, None, None)));
        assert_eq!(parse_stat_value(b""), Some((0, None, None)));
        assert_eq!(parse_stat_value(b",5"), Some((0, Some(5), None)));
        assert_eq!(parse_stat_value(b"5,"), Some((5, Some(0), None)));
        // Trailing junk is git's `error(_("invalid --stat value: %s"))`.
        assert_eq!(parse_stat_value(b"5x"), None);
        assert_eq!(parse_stat_value(b"5,6,7,8"), None);
    }

    /// `--stat-name-width` caps the filename column, and an over-long name is
    /// elided with `...` and re-anchored, exactly as `show_stats()` does. Verified
    /// against `git format-patch --stat-name-width=5`.
    #[test]
    fn stat_name_width_elides() {
        let files = [entry("abcdefghij", 1, 0)];
        let sw = StatWidths {
            width: 0,
            name_width: 5,
            graph_width: 0,
            count: 0,
        };
        assert_eq!(
            stats(&files, sw),
            " ...ij | 1 +\n 1 file changed, 1 insertion(+)\n"
        );
    }

    /// `--stat-count` lists only the first N files, appends git's ` ...` abbrev
    /// line, scales the columns to just the shown files, yet still counts every
    /// file in the insertions/deletions summary. Verified against
    /// `git format-patch --stat-count=2`.
    #[test]
    fn stat_count_truncates_but_totals_all() {
        let files = [entry("a", 2, 0), entry("bb", 0, 2), entry("ccc", 10, 10)];
        let sw = StatWidths {
            width: 0,
            name_width: 0,
            graph_width: 0,
            count: 2,
        };
        assert_eq!(
            stats(&files, sw),
            " a  | 2 ++\n bb | 2 --\n ...\n 3 files changed, 12 insertions(+), 12 deletions(-)\n"
        );
    }

    /// The all-zero widths reproduce format-patch's default 72-column diffstat,
    /// so a small unscaled change renders unchanged from before the port.
    #[test]
    fn default_widths_unscaled() {
        let files = [entry("x", 3, 1)];
        let sw = StatWidths {
            width: 0,
            name_width: 0,
            graph_width: 0,
            count: 0,
        };
        assert_eq!(
            stats(&files, sw),
            " x | 4 +++-\n 1 file changed, 3 insertions(+), 1 deletion(-)\n"
        );
    }

    /// xdiff records keep their newline, so `is_empty_rec()` must call a bare
    /// `"\n"` empty; and `def_ff()` accepts only a line whose *first* byte starts
    /// an identifier, which is what keeps an indented statement from being
    /// mistaken for a function header.
    #[test]
    fn funcname_and_empty_record_tests() {
        assert!(is_func_line(b"int one(void)\n"));
        assert!(is_func_line(b"_start:\n"));
        assert!(is_func_line(b"$var = 1\n"));
        assert!(!is_func_line(b"\tint a = 1;\n"));
        assert!(!is_func_line(b"}\n"));
        assert!(!is_func_line(b"\n"));
        assert!(!is_func_line(b""));

        assert!(is_empty_line(b"\n"));
        assert!(is_empty_line(b"   \t\n"));
        assert!(is_empty_line(b"\x0b\x0c"));
        assert!(!is_empty_line(b"}\n"));
    }

    /// `get_func_line()` takes its direction from the sign of `limit - start`,
    /// which is the whole reason one routine serves both the search for the
    /// function a hunk sits inside and the search for the next one down.
    #[test]
    fn get_func_line_scans_both_ways() {
        let lines: Vec<&[u8]> = vec![
            b"one(void)\n", // 0
            b"{\n",         // 1
            b"\tstmt;\n",   // 2
            b"}\n",         // 3
            b"\n",          // 4
            b"two(void)\n", // 5
            b"{\n",         // 6
            b"\tstmt;\n",   // 7
            b"}\n",         // 8
        ];
        // Backwards from inside `two` finds its header; the limit is exclusive.
        assert_eq!(get_func_line(&lines, 7, -1), 5);
        assert_eq!(get_func_line(&lines, 4, -1), 0);
        assert_eq!(get_func_line(&lines, 4, 1), -1);
        // Forwards from the end of `one`'s body finds the next header.
        assert_eq!(get_func_line(&lines, 3, lines.len() as i64), 5);
        // Nothing below the last header.
        assert_eq!(get_func_line(&lines, 6, lines.len() as i64), -1);
    }

    /// `diff_title()`: only a reroll count that parses as an integer >= 1 selects
    /// the "against v<n-1>" spelling, and the version named is the *previous*
    /// one. Captured from stock git 2.55.0, where `-v2 --interdiff=…` prints
    /// `Interdiff against v1:`.
    #[test]
    fn diff_title_follows_reroll_count() {
        let with = |reroll: Option<&str>| diff_title(reroll, "Interdiff:", "Interdiff against v");
        assert_eq!(with(None), "Interdiff:");
        assert_eq!(with(Some("2")), "Interdiff against v1:");
        assert_eq!(with(Some("1")), "Interdiff against v0:");
        // v0 (an RFC reroll) and a non-numeric count both fall back to the
        // generic title rather than naming a negative version.
        assert_eq!(with(Some("0")), "Interdiff:");
        assert_eq!(with(Some("rfc")), "Interdiff:");
    }

    /// git's `output_prefix` is written at the head of every emitted line, so the
    /// indent must reach a body's last line even when it has no trailing newline,
    /// and must not append one that git would not print.
    #[test]
    fn indent_prefixes_every_line() {
        let mut out = Vec::new();
        write_indented(&mut out, b"a\n\nb\n", 2);
        assert_eq!(out, b"  a\n  \n  b\n");

        let mut out = Vec::new();
        write_indented(&mut out, b"tail", 2);
        assert_eq!(out, b"  tail");

        // The cover letter renders the same patch flush left.
        let mut out = Vec::new();
        write_indented(&mut out, b"a\nb\n", 0);
        assert_eq!(out, b"a\nb\n");
    }
}
