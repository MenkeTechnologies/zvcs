use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::pretty_pad::{FlushType, PadState, WrapState};
use gix::bstr::ByteSlice;
use gix::date::time::Format as TimeFormat;
use gix::date::time::{CustomFormat, format as tfmt};
use gix::hash::ObjectId;
use gix::prelude::ObjectIdExt;
use regex::bytes::{Regex, RegexBuilder};

// ---------------------------------------------------------------------------
// usage blocks — one per `parse_options()` call in builtin/reflog.c.
//
// Every sub-command runs its own parser over its own `struct option options[]`,
// so once the sub-command word has been read `-h` is that sub-command's question
// and prints that sub-command's block: `git reflog expire -h` renders
// `reflog_expire_usage`, never `reflog_usage`. `--help-all` renders `USAGE_FULL`,
// which is the same block for all seven — no table here carries a
// `PARSE_OPT_HIDDEN` entry.
// ---------------------------------------------------------------------------

/// `cmd_reflog_show`'s block (builtin/reflog.c:40-43). Its table is `OPT_END()`
/// alone; everything else is forwarded to `cmd_log_reflog()`.
const SHOW_USAGE: &str = "\
usage: git reflog [show] [<log-options>] [<ref>]

";

/// `cmd_reflog_list`'s block (builtin/reflog.c:45-48), table `OPT_END()`.
const LIST_USAGE: &str = "\
usage: git reflog list

";

/// `cmd_reflog_exists`'s block (builtin/reflog.c:50-53), table `OPT_END()`.
const EXISTS_USAGE: &str = "\
usage: git reflog exists <ref>

";

/// `cmd_reflog_write`'s block (builtin/reflog.c:55-58), table `OPT_END()`.
const WRITE_USAGE: &str = "\
usage: git reflog write <ref> <old-oid> <new-oid> <message>

";

/// `cmd_reflog_delete`'s block over its table (builtin/reflog.c:310-321).
const DELETE_USAGE: &str = "\
usage: git reflog delete [--rewrite] [--updateref]
                         [--dry-run | -n] [--verbose] <ref>@{<specifier>}...

    -n, --[no-]dry-run    do not actually prune any entries
    --[no-]rewrite        rewrite the old SHA1 with the new SHA1 of the entry that now precedes it
    --[no-]updateref      update the reference to the value of the top reflog entry
    --[no-]verbose        print extra information on screen

";

/// `cmd_reflog_drop`'s block over its table (builtin/reflog.c:358-363).
const DROP_USAGE: &str = "\
usage: git reflog drop [--all [--single-worktree] | <refs>...]

    --[no-]all            drop the reflogs of all references
    --[no-]single-worktree
                          drop reflogs from the current worktree only

";

/// `cmd_reflog_expire`'s block over its table (builtin/reflog.c:190-213).
const EXPIRE_USAGE: &str = "\
usage: git reflog expire [--expire=<time>] [--expire-unreachable=<time>]
                         [--rewrite] [--updateref] [--stale-fix]
                         [--dry-run | -n] [--verbose] [--all [--single-worktree] | <refs>...]

    -n, --[no-]dry-run    do not actually prune any entries
    --[no-]rewrite        rewrite the old SHA1 with the new SHA1 of the entry that now precedes it
    --[no-]updateref      update the reference to the value of the top reflog entry
    --[no-]verbose        print extra information on screen
    --expire <timestamp>  prune entries older than the specified time
    --expire-unreachable <timestamp>
                          prune entries older than <time> that are not reachable from the current tip of the branch
    --[no-]stale-fix      prune any reflog entries that point to broken commits
    --[no-]all            process the reflogs of all references
    --[no-]single-worktree
                          limits processing to reflogs from the current worktree only

";

/// `usage_with_options()` over `builtin/reflog.c`'s subcommand table.
const USAGE: &str = r"usage: git reflog [show] [<log-options>] [<ref>]
   or: git reflog list
   or: git reflog exists <ref>
   or: git reflog write <ref> <old-oid> <new-oid> <message>
   or: git reflog delete [--rewrite] [--updateref]
                         [--dry-run | -n] [--verbose] <ref>@{<specifier>}...
   or: git reflog drop [--all [--single-worktree] | <refs>...]
   or: git reflog expire [--expire=<time>] [--expire-unreachable=<time>]
                         [--rewrite] [--updateref] [--stale-fix]
                         [--dry-run | -n] [--verbose] [--all [--single-worktree] | <refs>...]

";

/// `git reflog` — read the reference logs recorded under `$GIT_DIR/logs`.
///
/// Backed by gitoxide's `gix_ref` reflog reader (`Reference::log_iter()`), which
/// parses the raw `<old> <new> <sig>\t<message>` lines, plus a direct walk of the
/// log directory for the subcommands that are defined in terms of the files
/// themselves.
///
/// # Subcommands
///
///   * `git reflog [show] [<options>] [<ref>...]` — `show` is the default, and a
///     missing `<ref>` defaults to `HEAD`.
///   * `git reflog list` — every ref that has a reflog, in git's directory-tree
///     order (per-directory name sort).
///   * `git reflog exists <ref>` — exit 0 if `$GIT_DIR/logs/<ref>` is a file, else 1.
///   * `git reflog delete [--rewrite] [--updateref] [--dry-run] <ref>…` — drop
///     the named entries. The rest of the file is left byte-identical: the neighbours
///     keep the ids they recorded unless `--rewrite` closes the chain up, and the ref
///     only moves under `--updateref`. A selector past the end of a log is ignored.
///   * `git reflog expire [--expire=<t>] [--expire-unreachable=<t>] [--rewrite]
///     [--updateref] [--dry-run] [--verbose] [--all [--single-worktree] | <ref>…]` —
///     `should_expire_reflog_ent()`'s two tests: an entry goes when it is older than the
///     total cutoff, and also when it is older than the unreachable cutoff and neither of
///     its ids is reachable. What "reachable" means is
///     `reflog_expiry_prepare()`'s three regimes: every ref is a tip for `HEAD`
///     (`UE_HEAD`), the ref's own tip for anything else (`UE_NORMAL`), and nothing at all
///     once the unreachable cutoff is at or before the total one (`UE_ALWAYS`). The
///     cutoffs come from `--expire`/`--expire-unreachable`, then the first matching
///     `gc.<pattern>.reflogExpire[Unreachable]`, then `refs/stash`'s never-expire rule,
///     then `gc.reflogExpire[Unreachable]` and git's 90-day / 30-day defaults.
///     `--verbose` prints `keep`/`prune`/`would prune` per entry. `--all` covers every
///     worktree's logs unless `--single-worktree` narrows it. An emptied log is left as
///     an empty file, as git leaves it.
///   * `write` and `drop` bail — not ported.
///
/// # Argument grammar for `show`
///
/// `git reflog show` is `git log -g --abbrev-commit --pretty=oneline`, so it takes
/// the whole `git log` option vocabulary. Stock git processes argv strictly left to
/// right and resolves every non-option argument as a revision *as it is scanned*,
/// which fixes the error precedence reproduced here (verified against git 2.55.0):
///
///   1. `--date=<bogus>` / `--pretty=<bogus>` fail where they appear in argv.
///   2. A non-option argument that is not a revision fails at its own position —
///      *before* any option-validation error later in argv. `git reflog --verbose
///      does-not-exist` reports the ambiguous argument, not the bad flag.
///   3. Only after the whole scan: `--graph`/`--children`/`--topo-order`/
///      `--date-order`/`--author-date-order` report "cannot combine --walk-reflogs
///      with history-limiting options", which outranks `--reverse`'s own conflict.
///   4. Then `--reverse` reports its conflict with `--walk-reflogs`.
///   5. Then the first unrecognized option reports `unrecognized argument: <arg>`.
///
/// All five paths exit 128. `exists` without exactly one argument exits 129.
///
/// A non-option argument is read by the same `handle_revision_arg_1()` every
/// other verb gets, so the range and pathspec halves of that grammar reach
/// `show` too — and mostly reach it in order to be refused, because
/// `add_reflog_for_walk()` dies on any pending entry marked `UNINTERESTING`:
///
///   * `<a>..<b>` excludes its left endpoint, so it is
///     `fatal: cannot walk reflogs for <a>` (with `HEAD` standing in for an
///     endpoint written empty).
///   * `<a>...<b>` excludes the merge bases instead, and pends them ahead of
///     either endpoint under `oid_to_hex()`, so the name in that message is a
///     full-length object id. Two histories with no merge base at all exclude
///     nothing and walk *both* reflogs.
///   * An endpoint that is not an `OBJ_COMMIT` — an annotated tag — is pended
///     and dropped rather than walked, so it neither dies nor contributes.
///   * A bare `..` is not a range but the pathspec for the parent directory
///     (revision.c:2164): it and every operand behind it become prune data, and
///     the diagnostic comes from `pathspec.c`.
///
/// See [`dotdot_walks`] for the C those four bullets are read off.
///
/// # Implemented `show` options
///
/// Counting: `-n <n>`, `-n<n>`, `-<n>`, `--max-count=<n>`, `--skip=<n>` — one budget
/// shared across every ref, applied after filtering, as in git.
///
/// Abbreviation: `--abbrev=<n>`, `--no-abbrev`, `--abbrev-commit`, `--no-abbrev-commit`.
///
/// Ref sets: `--all`, `--branches[=<pat>]`, `--tags[=<pat>]`, `--remotes[=<pat>]`,
/// `--glob=<pat>`, `--exclude=<pat>` (applies to the ref-set options that follow it).
/// Patterns use git's wildmatch with `*` crossing `/`, and a pattern without a `*`
/// gains a trailing `/*`, matching `normalize_glob_ref()`.
///
/// Selector display: `--date=<fmt>` for `default`, `raw`, `unix`, `short`, `iso`,
/// `iso8601`, `iso-strict`, `iso8601-strict`, `rfc`, `rfc2822`, `local`, and the
/// `-local` variant of each. A `<ref>@{<date>}` argument also switches the selector
/// to date form, as git does. `local` re-anchors the entry's timestamp to the zone
/// named by `$TZ` (or `/etc/localtime`), read straight out of the TZif database.
///
/// `log.date` supplies the default *field* date format — the one used for the
/// `%ad`/`%cd` placeholders and the `Date:`/`AuthorDate:`/`CommitDate:` header
/// lines — which `--date=` then overrides. It never changes the reflog selector
/// column (that stays in count form unless an explicit `--date=` or a `@{<date>}`
/// argument switches it), and git validates it before argv, so an unknown value is
/// fatal ahead of any option or revision error. Its `relative`, `human` and
/// `format:...` modes are deferred exactly like `--date`'s: the command still
/// succeeds whenever nothing renders a field date.
///
/// Filtering: `--merges`, `--no-merges` (by parent count of the entry's commit).
/// `--since=`/`--after=` and `--until=`/`--before=` keep entries by their own
/// reflog timestamp — the instant the ref was updated, which is what git's `-g`
/// walk limits on via the fake reflog parent — parsed through git's approxidate
/// (`1 year ago`, `now`, …), inclusive at both ends. Pathspecs after `--` keep an
/// entry only when its commit's diff against its first parent touches one of them.
///
/// Decoration: `--decorate[=short|full]` annotates each entry's commit with the
/// refs (`refs/heads`, `refs/remotes`, `refs/tags`, `HEAD`) that resolve to it,
/// in git's order — descending full-ref-name, `HEAD` first as `HEAD -> <branch>`
/// or bare `HEAD`. `--decorate=auto`/`--no-decorate`/`--decorate=no` are off, as
/// the default is when stdout is not a tty.
///
/// Output: `--parents`, and `--format=`/`--pretty=` for the placeholders
/// `%H %h %T %P %p %s %an %ae %ad %cn %ce %cd %gd %gD %gn %ge %gs %n %% %x<hh>`,
/// the column atoms `%<(<N>)`, `%>(<N>)`, `%><(<N>)`, `%>>(<N>)` (with their
/// `%<|(<N>)` column-target and `,trunc`/`,ltrunc`/`,mtrunc` forms) and the
/// `%w(<width>,<indent1>,<indent2>)` wrap atom — all through the shared
/// [`super::pretty_pad`] port, so a field is measured in display columns and a
/// CJK subject costs two per glyph. `%C…` is refused below, so
/// `format_and_pad_commit()`'s colour chain can never open here.
/// Also supported is the `oneline` built-in. Empty formats print nothing at all, and a format
/// string is newline-terminated per entry, both matching git. The multi-line
/// built-ins `medium` (also bare `--pretty`), `short`, `full`, `fuller`, `raw` and
/// `reference` render with git's `Reflog:`/`Reflog message:` header lines; only
/// the `email`/`mboxrd` patch formats remain deferred.
///
/// Filtering: `--grep=<pat>` keeps entries whose commit message matches, with
/// git's default POSIX-basic dialect (translated to the `regex` engine), plus
/// `-E`/`-P` (extended), `-F` (fixed), `-i` (ignore case), `--all-match` and
/// `--invert-grep`. A pattern git's regex compiler would reject is fatal (128),
/// though the message names only "invalid regular expression" where git quotes
/// its regex library's own diagnosis.
///
/// `--grep-reflog=<pat>` matches the reflog entry's own message. git compiles it
/// as a `reflog` *header* grep, which has three consequences this module
/// reproduces: an entry must satisfy the header patterns *and* the `--grep`
/// patterns, the header patterns stay OR-ed even under `--all-match`, and
/// `--invert-grep` inverts only the message side.
///
/// # Diff output
///
/// `--raw`, `--numstat`, `--summary`, `--shortstat`, `--name-only`,
/// `--name-status`, `--stat` and `-p`/`--patch` render the diff of each entry's
/// commit against its first parent (the empty tree for a root commit) — the last
/// two through the same renderers `diff --stat` and `log -p` use, so a patch here
/// is the patch there. Merge commits produce no diff, matching `git log`'s
/// default of not diffing a merge at all, unless `--first-parent` picks a side —
/// which is how `stash list -p` shows a stash entry's own change. Paths go through git's
/// `quote_c_style()`, honouring `core.quotePath`, and renames through its
/// `pprint_rename()` brace compaction. `--raw` object ids are abbreviated with the
/// diff `--abbrev`, a missing side printed as an abbreviated null id.
///
/// git's output-format bits behave in a specific, order-sensitive way that is
/// reproduced here (verified against git 2.55.0): `--raw`, `--name-only`,
/// `--name-status`, `--numstat`, `--summary` and `--shortstat` each *add* a bit,
/// while `-s`/`--no-patch` *assigns* "no output", clearing every bit set before it.
/// After the scan, more than one of `--name-only`/`--name-status`/`-s` is fatal,
/// and either name format suppresses both the stat family and `--raw`. So
/// `--numstat -s` prints nothing while `-s --numstat` prints the numstat.
///
/// # Options recognized but deliberately not implemented
///
/// These bail with a terse reason rather than being ignored, because ignoring them
/// would print a wrong answer that looks like success:
///
///   * Diff output that needs the rest of git's diff driver — `-p`, `--patch`,
///     `--stat` (column-width scaling against the terminal width), `--dirstat`.
///   * `%C(...)` color placeholders and `--color=always`. (The `%d`/`%D` ref
///     decorations and the `%ar`/`%cr` relative and `%ai`/`%at` date atoms are
///     supported.)
///   * The `email`/`mboxrd` patch `--pretty` formats, which need git's mbox driver.
///     These are deferred: when a filter (a date limiter or a pathspec) drops every
///     entry the format is never exercised and the command succeeds with empty
///     output, exactly as git does.
///   * `--date=relative`, `--date=human`, `--date=format:...` — these need the
///     current time or strftime-style user formats, which `gix-date` does not expose.
///
/// # Known divergences
///
///   * `--all` and `--glob` group entries per ref, in ref-name order, with `HEAD`
///     last. Git feeds all reflogs through its date-ordered revision walk, so when
///     reflogs of different refs interleave in time the orders differ. They agree
///     whenever each ref's entries form one contiguous run, which is the common case.
///   * `--abbrev=<n>` emits exactly `n` hex characters; git would lengthen the
///     prefix further if `n` were not unique. Automatic abbreviation (the default)
///     does go through gitoxide's disambiguating `shorten()`.
///   * When a reflog entry names an object missing from the odb, `shorten()` fails
///     and the id falls back to a plain [`abbrev_len`]-length prefix, and the
///     commit-derived placeholders (`%s`, `%an`, …) render empty instead of git's
///     fatal error.
///   * A rename below 100% similarity reports `gix-diff`'s byte-ratio score, while
///     git reports its own `estimate_similarity()` score over hashed chunks. The two
///     agree at 100% (identical blob ids) and can differ by a percent otherwise.
///   * Pathspec filtering matches an entry against the diff of its commit versus
///     its first parent, so a merge entry (which this module does not diff) is
///     dropped by any pathspec. git simplifies merge history against a pathspec
///     differently; the two agree on the non-merge entries that dominate a reflog.
pub fn reflog(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand being present at index 0 regardless of how the
    // dispatcher slices argv.
    let args: &[String] = match args.first() {
        Some(a) if a == "reflog" => &args[1..],
        _ => args,
    };

    // `cmd_reflog`'s `parse_options(..., PARSE_OPT_SUBCOMMAND_OPTIONAL)` scans
    // leading options and stops at the first non-option, which becomes the
    // subcommand. So `-h` is this command's help exactly while it is the FIRST
    // token — the subcommand synopsis on stdout, exit 129. Once a subcommand has
    // been named, `-h` belongs to that subcommand's own parser instead.
    // `--help-all` answers the same way: parse_options_step() tests it with a
    // `strcmp()` of its own ahead of parse_long_opt(), and renders `USAGE_FULL`
    // — identical here because this option table has no `PARSE_OPT_HIDDEN`
    // entry. The compare is exact, which is why `--help-a` and `--help-all=x`
    // stay `unrecognized argument` reports.
    if args.first().is_some_and(|a| a == "-h" || a == "--help-all") {
        return Ok(super::show_usage(USAGE));
    }

    let (sub, rest): (&str, &[String]) = match args.first().map(String::as_str) {
        Some("show") => ("show", &args[1..]),
        Some("list") => ("list", &args[1..]),
        Some("exists") => ("exists", &args[1..]),
        Some("delete") => ("delete", &args[1..]),
        Some("expire") => ("expire", &args[1..]),
        Some("drop") => ("drop", &args[1..]),
        Some("write") => ("write", &args[1..]),
        // Anything else is a `<ref>` for the implicit `show`.
        _ => ("show", args),
    };

    let mut repo = gix::discover(".")?;
    match sub {
        "show" => {
            // `cmd_reflog_show`'s table is empty, so no token is ever a value and
            // each is tested on its own. `PARSE_OPT_KEEP_DASHDASH` leaves the `--`
            // in argv for the revision parser but still breaks the option loop,
            // so nothing past it asks for help.
            if rest
                .iter()
                .take_while(|a| a.as_str() != "--")
                .any(|a| super::asks_for_help(a, ""))
            {
                return Ok(super::show_usage(SHOW_USAGE));
            }
            show(&repo, rest, Tweak::Reflog).map(ExitCode::from)
        }
        "list" => list(&repo, rest),
        "exists" => exists(&repo, rest),
        "delete" => delete_entries(&repo, rest),
        "expire" => expire_entries(&repo, rest),
        "drop" => drop_reflogs(&repo, rest),
        "write" => write_reflog(&mut repo, rest),
        _ => unreachable!("subcommand set is closed above"),
    }
}

/// `git log -g`'s reflog walk, for a caller that is `log` rather than `reflog`.
///
/// The two entry points differ in exactly one place. `cmd_log` installs
/// `log_setup_revisions_tweak`, which calls `diff_merges_default_to_first_parent`
/// when `--first-parent` was given; `cmd_log_reflog` (`builtin/log.c`) installs
/// no tweak at all. So `git reflog show -p --first-parent` prints no merge diff
/// while `git log -g -p --first-parent` does — and `list_stash()` runs the
/// latter. Every stash entry is a merge commit, so that tweak is the only reason
/// `git stash list -p` renders a patch at all.
pub fn reflog_show_as_log(args: &[String]) -> Result<ExitCode> {
    reflog_show_as_log_status(args).map(ExitCode::from)
}

/// [`reflog_show_as_log`] with the status still readable as a number.
///
/// `cmd_stash` returns `!!fn(argc, argv, prefix, repo)` (builtin/stash.c:2496),
/// so every sub-command's *return* value is squashed to 0 or 1 before it leaves
/// the process: `git stash list --zzbogus` exits 1 even though the `git log` it
/// spawned exited 128. Only the paths that `exit()` on their own — `die()` and
/// `usage_with_options()` — keep their own status, which is why
/// `git stash show --zzbogus` still exits 129. `std::process::ExitCode` cannot be
/// read back, so `list_stash()`'s caller needs the number rather than the code.
pub fn reflog_show_as_log_status(args: &[String]) -> Result<u8> {
    let repo = gix::discover(".")?;
    show(&repo, args, Tweak::Log)
}

/// Which of git's two reflog-walk entry points is running.
///
/// Named after the `setup_revision_opt::tweak` hook that is the actual
/// difference between them; see [`reflog_show_as_log`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tweak {
    /// `git reflog show`: no tweak, so `--first-parent` never diffs a merge.
    Reflog,
    /// `git log -g`: `--first-parent` promotes merges to a first-parent diff.
    Log,
}

/// One reflog line, already flipped into git's newest-first order.
struct Entry {
    oid: ObjectId,
    who_name: Vec<u8>,
    who_email: Vec<u8>,
    time: gix::date::Time,
    message: Vec<u8>,
}

/// One ref's worth of reflog, plus how it should be named in the output.
struct Section {
    /// The ref as it should be printed: as typed for an explicit argument, the
    /// full name for `--all`/`--glob`, the short name for `--branches` and friends.
    display: String,
    /// The full ref name, for `%gD`.
    full: String,
    /// Index of the first entry to print (a `@{<n>}` or `@{<date>}` start point).
    start: usize,
    /// Which selector form this argument used, which decides the `@{…}` column
    /// for this section alone (git decides it per argument, not once for the
    /// whole command).
    selector: SelectorKind,
    entries: Vec<Entry>,
}

/// git's `enum selector_type` (`reflog-walk.c`), recorded per argument.
///
/// It is what decides the `@{…}` column: `get_reflog_selector` prints a date
/// only for [`SelectorKind::Date`], or for [`SelectorKind::None`] when `--date=`
/// was given explicitly. An `@{<n>}` argument is [`SelectorKind::Index`] and
/// keeps counting even under `--date=` — so `reflog main@{1} --date=unix` prints
/// `main@{1}`, not `main@{1700000000}`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectorKind {
    /// No `@{…}` was typed: the column counts, unless `--date=` forces dates.
    None,
    /// `<ref>@{<n>}`: the column counts, whatever `--date=` says.
    Index,
    /// `<ref>@{<date>}`: the column shows dates, whatever `--date=` says.
    Date,
}

/// How commit ids are rendered.
enum Abbrev {
    /// git's automatic length: the shortest unique prefix, at least `core.abbrev`.
    Auto,
    /// Exactly this many hex characters.
    Len(usize),
    /// The whole hash.
    Full,
}

/// A `--date=` selection: which layout, and whether to re-anchor to the local zone.
#[derive(Clone, Copy)]
struct DateFormat {
    fmt: TimeFormat,
    local: bool,
    /// git's `iso-strict` mode, which prints `Z` (not `+00:00`) at a zero UTC
    /// offset. gitoxide's `ISO8601_STRICT` always spells the offset out, so this
    /// flag drives a post-format fixup of the zero-offset case.
    iso_strict: bool,
}

impl DateFormat {
    fn plain(fmt: impl Into<TimeFormat>) -> Self {
        DateFormat {
            fmt: fmt.into(),
            local: false,
            iso_strict: false,
        }
    }

    /// Render `time`, first moving it into the local zone when `--date=…-local`.
    fn render(self, time: gix::date::Time) -> String {
        let time = if self.local {
            gix::date::Time::new(time.seconds, local_offset(time.seconds))
        } else {
            time
        };
        let out = time.format_or_unix(self.fmt);
        // `git`'s ISO-8601-strict layout uses a literal `Z` for UTC, where
        // gitoxide's `%:z` renders `+00:00`.
        if self.iso_strict {
            if let Some(prefix) = out.strip_suffix("+00:00") {
                return format!("{prefix}Z");
            }
        }
        out
    }
}

/// git's `DEFAULT` layout without the trailing ` %z`, which is what every `-local`
/// rendering of the default mode prints.
const DEFAULT_LOCAL: CustomFormat = CustomFormat::new("%a %b %-d %H:%M:%S %Y");

/// git's `output_format` bits, minus the ones this module does not render.
#[derive(Default, Clone, Copy)]
struct DiffFormats {
    name_only: bool,
    name_status: bool,
    numstat: bool,
    shortstat: bool,
    summary: bool,
    /// git's `DIFF_FORMAT_RAW`, set by `--raw`: `:<mode> <mode> <sha> <sha>
    /// <status>\t<path>`. Not one of the mutually-exclusive bits, but a name
    /// format still supersedes it.
    raw: bool,
    /// git's `DIFF_FORMAT_NO_OUTPUT`, set by `-s`/`--no-patch`. It renders nothing
    /// itself but still counts towards the "cannot be used together" check.
    no_output: bool,
    /// git's `DIFF_FORMAT_PATCH`, set by `-p`/`-u`/`--patch`: the body is rendered
    /// by the same machinery `log -p` uses.
    patch: bool,
    /// git's `DIFF_FORMAT_DIFFSTAT`, set by `--stat`/`--patch-with-stat`,
    /// rendered by the same histogram `diff --stat` prints.
    stat: bool,
}

impl DiffFormats {
    fn any(self) -> bool {
        self.name_only
            || self.name_status
            || self.numstat
            || self.shortstat
            || self.summary
            || self.raw
            || self.patch
            || self.stat
    }

    /// `-s` / `--no-patch` assigns "no output", dropping every bit set before it.
    fn set_no_output(&mut self) {
        *self = DiffFormats {
            no_output: true,
            ..DiffFormats::default()
        };
    }

    /// Whether a patch body is to be rendered after the name/stat formats, which
    /// is git's ordering when both are asked for.
    fn wants_patch(self) -> bool {
        self.patch && !self.no_output
    }

    /// The bits git's `HAS_MULTI_BITS()` check counts.
    fn exclusive_bits(self) -> usize {
        usize::from(self.name_only) + usize::from(self.name_status) + usize::from(self.no_output)
    }

    /// git's `diff_setup_done()`: either name format outranks the stat family
    /// and the raw format.
    fn resolve(&mut self) {
        if self.name_only || self.name_status {
            self.numstat = false;
            self.shortstat = false;
            self.summary = false;
            self.raw = false;
            // A name format replaces `DIFF_FORMAT_PATCH`/`DIFFSTAT` rather than
            // joining them.
            self.patch = false;
            self.stat = false;
        }
    }
}

struct Opts {
    max_count: Option<usize>,
    skip: usize,
    abbrev: Abbrev,
    /// Set by `--date=<fmt>`.
    date: Option<DateFormat>,
    /// `log.date`: the default field date format for `%ad`/`%cd` and the
    /// `Date:`/`AuthorDate:`/`CommitDate:` header lines, used when no `--date=`
    /// overrides it. It never touches the reflog selector column, which stays in
    /// count form unless an explicit `--date=` or a `@{<date>}` argument switches
    /// it to date form.
    log_date: Option<DateFormat>,
    /// A recognized but unrenderable `log.date` mode (`relative`/`human`/
    /// `format:...`). Deferred like the other unimplemented options: it only fails
    /// when an entry is actually printed in a format that renders a field date,
    /// and only when no `--date=` overrode it.
    log_date_unsupported: Option<String>,
    /// The output layout: `--oneline` (git's default for reflog), a `--format=`/
    /// `--pretty=<placeholders>` string, or a built-in multi-line format.
    out: OutFmt,
    /// `--grep=<pat>` message filters (matched against each entry's commit
    /// message); `None` when no `--grep` was given.
    grep: Option<GrepFilter>,
    parents: bool,
    /// `--first-parent`: diff a merge entry against its first parent instead of
    /// skipping it, which is what makes `stash list` show a stash's diff.
    first_parent: bool,
    /// `Some(true)` for `--merges`, `Some(false)` for `--no-merges`.
    merges: Option<bool>,
    diff: DiffFormats,
    /// `--decorate[=short|full|auto|no]` — how to annotate each entry's commit
    /// with the refs that point at it. `None` is git's piped default (off).
    decorate: Option<Decorate>,
    /// `--since=`/`--after=`: keep entries whose own timestamp is `>=` this instant.
    since: Option<i64>,
    /// `--until=`/`--before=`: keep entries whose own timestamp is `<=` this instant.
    until: Option<i64>,
    /// Pathspecs after `--`: keep an entry only when its commit's diff against its
    /// first parent touches at least one of them.
    pathspecs: Vec<Vec<u8>>,
}

/// `--decorate` rendering mode. `Short` strips the ref namespace prefix, `Full`
/// keeps the whole ref name; both prefix tags with `tag: `.
#[derive(Clone, Copy)]
enum Decorate {
    Short,
    Full,
}

/// The reflog output layout.
enum OutFmt {
    /// `git reflog`'s default (`--pretty=oneline` with `--abbrev-commit`).
    Oneline,
    /// A `--format=`/`--pretty=<placeholders>` user string.
    Custom(String),
    /// A named multi-line format that carries git's reflog decorations.
    Builtin(Builtin),
}

/// A `git log` built-in `--pretty` format, minus `oneline` (its own variant) and
/// the `email`/`mboxrd` patch formats (still deferred as unimplemented).
#[derive(Clone, Copy)]
enum Builtin {
    Medium,
    Short,
    Full,
    Fuller,
    Raw,
    Reference,
}

impl Builtin {
    /// Whether git prints a blank line between consecutive entries. The header
    /// formats do; `reference` is one-line-like and does not.
    fn separates(self) -> bool {
        !matches!(self, Builtin::Reference)
    }
}

/// `--grep=` message filtering, matched against each entry's commit message the
/// way git's `--walk-reflogs` grep does (with `--all-match` / `--invert-grep`).
struct GrepFilter {
    patterns: Vec<Regex>,
    /// `--grep-reflog=<pat>`. git adds these to the same filter as a `reflog `
    /// *header* grep, so they are matched against the reflog entry's own
    /// message rather than the commit's, and they combine with `--grep` under
    /// the same any/all rule.
    reflog_patterns: Vec<Regex>,
    /// `--all-match`: every pattern must match instead of any.
    all_match: bool,
    /// `--invert-grep`: keep entries that do *not* match.
    invert: bool,
}

impl GrepFilter {
    /// Whether one entry survives the filter.
    ///
    /// git compiles the message patterns and the `reflog` header patterns into
    /// separate expressions and sets `all_match` once a header expression
    /// exists, so an entry has to satisfy both groups. Within the header group
    /// the patterns stay OR-ed even under `--all-match`, and `--invert-grep`
    /// (git's `no_body_match`) rejects on a *body* hit only, leaving header
    /// matching untouched.
    fn keeps(&self, message: &[u8], reflog_message: &[u8]) -> bool {
        let body_ok = if self.patterns.is_empty() {
            true
        } else {
            let hit = if self.all_match {
                self.patterns.iter().all(|re| re.is_match(message))
            } else {
                self.patterns.iter().any(|re| re.is_match(message))
            };
            hit != self.invert
        };
        let header_ok = self.reflog_patterns.is_empty()
            || self
                .reflog_patterns
                .iter()
                .any(|re| re.is_match(reflog_message));
        body_ok && header_ok
    }
}

/// git's default `--grep` dialect selection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GrepKind {
    /// POSIX basic regular expressions (git's default).
    Basic,
    /// `-E`/`-P`: extended/Perl — passed to the (ERE-superset) `regex` engine.
    Extended,
    /// `-F`: a literal string.
    Fixed,
}

/// Translate a POSIX **basic** regular expression to the `regex` crate's dialect
/// (an ERE superset). In BRE `+ ? | ( ) { }` are literal and their backslashed
/// forms are the operators; `. * [ ] ^ $ \` mean the same in both. This swaps the
/// two escaping conventions and leaves bracket expressions untouched.
fn bre_to_ere(pat: &str) -> String {
    let mut out = String::new();
    let mut chars = pat.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                // Backslashed operator in BRE -> bare operator in ERE.
                Some(n @ ('+' | '?' | '|' | '(' | ')' | '{' | '}')) => out.push(n),
                // Same meaning in both dialects; keep the escape.
                Some(n @ ('.' | '*' | '[' | ']' | '^' | '$' | '\\')) => {
                    out.push('\\');
                    out.push(n);
                }
                // Shared character-class shorthands.
                Some(n @ ('w' | 's' | 'b' | 'd' | 'B' | 'S' | 'W')) => {
                    out.push('\\');
                    out.push(n);
                }
                // `\<other>` is a literal `<other>` in BRE.
                Some(n) => out.push_str(&regex::escape(&n.to_string())),
                None => out.push_str("\\\\"),
            },
            // Literal in BRE, operator in ERE: escape to keep it literal.
            '+' | '?' | '|' | '(' | ')' | '{' | '}' => {
                out.push('\\');
                out.push(c);
            }
            // Copy a bracket expression verbatim (identical in BRE and ERE).
            '[' => {
                out.push('[');
                if chars.peek() == Some(&'^') {
                    out.push(chars.next().expect("peeked"));
                }
                // A `]` immediately after `[` or `[^` is a literal member.
                if chars.peek() == Some(&']') {
                    out.push(chars.next().expect("peeked"));
                }
                for d in chars.by_ref() {
                    out.push(d);
                    if d == ']' {
                        break;
                    }
                }
            }
            // `.` `*` `^` `$` and every ordinary character mean the same thing.
            _ => out.push(c),
        }
    }
    out
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            max_count: None,
            skip: 0,
            abbrev: Abbrev::Auto,
            date: None,
            log_date: None,
            log_date_unsupported: None,
            out: OutFmt::Oneline,
            grep: None,
            parents: false,
            first_parent: false,
            merges: None,
            diff: DiffFormats::default(),
            decorate: None,
            since: None,
            until: None,
            pathspecs: Vec::new(),
        }
    }
}

/// Apply a `--skip`/`--max-count` value, reporting git's `not an integer` fatal
/// and returning `false` when the value is one `strtol_i` rejects.
///
/// Both slots are signed `int`s in `struct rev_info`, and the walk only ever
/// tests them positively (`if (revs->max_count == 0) return NULL;` /
/// `if (revs->skip_count > 0)`), so a negative value is accepted and means "no
/// limit" for `--max-count` and "skip nothing" for `--skip`. Shared with
/// `git log`'s parser so the two agree on every spelling.
fn set_count(opts: &mut Opts, is_skip: bool, value: &str) -> bool {
    if is_skip {
        match super::log::parse_skip(value) {
            Ok(n) => opts.skip = n,
            Err(()) => {
                eprintln!("fatal: '{value}': not an integer");
                return false;
            }
        }
    } else {
        match super::log::parse_max_count(value) {
            Ok(n) => opts.max_count = n,
            Err(()) => {
                eprintln!("fatal: '{value}': not an integer");
                return false;
            }
        }
    }
    true
}

/// Record the first option that is recognized but not rendered here. Only the
/// first matters: git would have failed on it before reaching any later one.
fn note_first(slot: &mut Option<String>, what: String) {
    if slot.is_none() {
        *slot = Some(what);
    }
}

/// Resolve a `--since`/`--until` value to a unix instant the way git's `approxidate()` does —
/// through the one shared parser, which never errors on a date limiter.
fn parse_limit_date(value: &str) -> i64 {
    crate::date::approxidate(value)
}

/// git pathspec match: an entry survives when at least one of its changed paths (a
/// destination, or a rename/copy source) equals a pathspec or lies under it.
fn pathspec_matches(changes: &[FileChange], specs: &mut super::log::PathspecMatcher) -> bool {
    changes.iter().any(|change| {
        specs.matches(&change.path)
            || change.source.as_deref().is_some_and(|s| specs.matches(s))
    })
}

/// The refs that decorate each commit, resolved once for a `--decorate` run.
struct Decorations {
    /// Peeled commit id -> the full names of every ref that resolves to it.
    by_oid: HashMap<ObjectId, Vec<String>>,
    /// The commit `HEAD` resolves to, when there is one.
    head_oid: Option<ObjectId>,
    /// The branch `HEAD` symrefs to when attached, as a full ref name.
    head_branch: Option<String>,
    mode: Decorate,
}

impl Decorations {
    fn build(repo: &gix::Repository, mode: Decorate) -> Self {
        let mut by_oid: HashMap<ObjectId, Vec<String>> = HashMap::new();
        if let Ok(platform) = repo.references() {
            if let Ok(iter) = platform.all() {
                for reference in iter.flatten() {
                    let name = reference.name().as_bstr().to_str_lossy().into_owned();
                    if !(name.starts_with("refs/heads/")
                        || name.starts_with("refs/remotes/")
                        || name.starts_with("refs/tags/"))
                    {
                        continue;
                    }
                    if let Ok(id) = reference.into_fully_peeled_id() {
                        by_oid.entry(id.detach()).or_default().push(name);
                    }
                }
            }
        }
        let head = repo.head().ok();
        let head_branch = head.as_ref().and_then(|h| {
            (!h.is_detached())
                .then(|| h.referent_name().map(|n| n.as_bstr().to_str_lossy().into_owned()))
                .flatten()
        });
        let head_oid = repo.head_id().ok().map(|id| id.detach());
        Decorations {
            by_oid,
            head_oid,
            head_branch,
            mode,
        }
    }

    /// The parenthesised decoration for a commit, or `None` when nothing points at
    /// it. git prepends each ref as it walks the sorted ref list, so the refs come
    /// out in descending full-name order; `HEAD` is placed first, as `HEAD ->
    /// <branch>` when it symrefs to a branch at this commit, else a bare `HEAD`.
    fn for_commit(&self, oid: ObjectId) -> Option<String> {
        self.bare_for_commit(oid).map(|inner| format!("({inner})"))
    }

    /// The decoration content without the surrounding parentheses — the `%D` form
    /// (`HEAD -> main, tag: v1`). `%d` wraps this in ` (...)`, and the oneline
    /// `--decorate` output uses the parenthesised [`for_commit`](Self::for_commit).
    fn bare_for_commit(&self, oid: ObjectId) -> Option<String> {
        let mut names: Vec<String> = self.by_oid.get(&oid).cloned().unwrap_or_default();
        names.sort();
        names.reverse();

        let mut items: Vec<String> = Vec::with_capacity(names.len() + 1);
        if self.head_oid == Some(oid) {
            match &self.head_branch {
                Some(branch) => {
                    names.retain(|n| n != branch);
                    items.push(format!("HEAD -> {}", self.render_ref(branch)));
                }
                None => items.push("HEAD".to_owned()),
            }
        }
        for name in &names {
            items.push(self.decorate_ref(name));
        }
        if items.is_empty() {
            return None;
        }
        Some(items.join(", "))
    }

    /// A ref name shortened per the decorate mode, without the `tag:` prefix.
    fn render_ref(&self, name: &str) -> String {
        match self.mode {
            Decorate::Full => name.to_owned(),
            Decorate::Short => name
                .strip_prefix("refs/heads/")
                .or_else(|| name.strip_prefix("refs/remotes/"))
                .or_else(|| name.strip_prefix("refs/tags/"))
                .unwrap_or(name)
                .to_owned(),
        }
    }

    /// A ref as it appears in the decoration list: tags carry a `tag: ` prefix.
    fn decorate_ref(&self, name: &str) -> String {
        if name.starts_with("refs/tags/") {
            format!("tag: {}", self.render_ref(name))
        } else {
            self.render_ref(name)
        }
    }
}

/// `git reflog show` — render the log of each `<ref>` (default `HEAD`).
fn show(repo: &gix::Repository, rest: &[String], tweak: Tweak) -> Result<u8> {
    let full_hex = repo.object_hash().len_in_hex();
    // `core.quotePath` is read once, into the flag every `quote_c_style()` caller
    // shares, exactly as `git_default_core_config()` does.
    crate::quote::init(repo);
    let mut opts = Opts::default();

    // git validates `log.date` in its log-config callback, which runs before the
    // argument scan, so an unknown value is fatal ahead of any option or revision
    // error (verified against git 2.55.0). An empty value is unknown too, where
    // `parse_date_mode("")` would otherwise accept it as the default layout.
    if let Some(raw) = repo.config_snapshot().string("log.date") {
        let value = raw.to_str_lossy().into_owned();
        match if value.is_empty() {
            DateMode::Unknown
        } else {
            parse_date_mode(&value)
        } {
            DateMode::Known(f) => opts.log_date = Some(f),
            DateMode::Unimplemented => opts.log_date_unsupported = Some(value),
            DateMode::Unknown => {
                eprintln!("fatal: unknown date format {value}");
                return Ok(128);
            }
        }
    }

    let mut sections: Vec<Section> = Vec::new();
    let mut excludes: Vec<String> = Vec::new();
    // Whether argv named any reflog to read; if not, `show` defaults to HEAD.
    let mut saw_ref_source = false;
    let mut limited = false;
    let mut reverse = false;
    let mut unrecognized: Option<String> = None;
    let mut unimplemented: Option<String> = None;

    // `--grep` state, resolved into a compiled filter after the whole scan (git
    // sets these fields in any order, then compiles once in `setup_revisions`).
    let mut grep_patterns: Vec<String> = Vec::new();
    let mut grep_reflog_patterns: Vec<String> = Vec::new();
    let mut grep_kind = GrepKind::Basic;
    let mut grep_ignore_case = false;
    let mut grep_invert = false;
    let mut grep_all_match = false;

    // `setup_revisions()` searches the *whole* argv for a `--` before it reads
    // any argument (revision.c:2836-2851), and every surviving argument then
    // carries `REVARG_CANNOT_BE_FILENAME`. That flag is the only thing standing
    // between a bare `..` and the parent-directory pathspec, so the scan has to
    // happen up front rather than when the `--` is reached below.
    let seen_dashdash = rest.iter().any(|s| s == "--");

    let mut i = 0;
    while i < rest.len() {
        let a = rest[i].as_str();
        match a {
            // ---- end of options --------------------------------------------
            // Everything after the first `--` is a pathspec, including a further
            // literal `--`. git resolves none of these as revisions.
            "--" => {
                opts.pathspecs = rest[i + 1..].iter().map(|s| s.as_bytes().to_vec()).collect();
                break;
            }

            // ---- counting -------------------------------------------------
            "-n" | "--max-count" | "--skip" => {
                i += 1;
                let Some(v) = rest.get(i) else {
                    if a == "-n" {
                        eprintln!("error: -n requires an argument");
                    } else {
                        eprintln!("error: option `{}' requires a value", &a[2..]);
                    }
                    return Ok(128);
                };
                if !set_count(&mut opts, a == "--skip", v) {
                    return Ok(128);
                }
            }
            s if s.starts_with("--max-count=") || s.starts_with("--skip=") => {
                let (key, v) = s.split_once('=').expect("checked for `=` above");
                if !set_count(&mut opts, key == "--skip", v) {
                    return Ok(128);
                }
            }
            // `-n<value>`: `skip_prefix(arg, "-n", &optarg)` in `revision.c`, so
            // the rest of the token is the value whatever it looks like — `-n-1`
            // is unlimited and `-nabc` is fatal, neither of them an unknown option.
            s if s.len() > 2 && s.starts_with("-n") => {
                if !set_count(&mut opts, false, &s[2..]) {
                    return Ok(128);
                }
            }
            s if s.len() > 1 && s.starts_with('-') && all_digits(&s[1..]) => {
                opts.max_count = Some(s[1..].parse().expect("all digits"));
            }

            // ---- abbreviation ---------------------------------------------
            "--abbrev" | "--abbrev-commit" => opts.abbrev = Abbrev::Auto,
            "--no-abbrev" | "--no-abbrev-commit" => opts.abbrev = Abbrev::Full,
            s if s.starts_with("--abbrev=") => {
                // git clamps to [4, hash-len] and treats garbage as the minimum.
                let n = s["--abbrev=".len()..].parse::<usize>().unwrap_or(0);
                opts.abbrev = Abbrev::Len(n.clamp(4, full_hex));
            }

            // ---- selector date format -------------------------------------
            s if s.starts_with("--date=") => match parse_date_mode(&s["--date=".len()..]) {
                DateMode::Known(f) => opts.date = Some(f),
                DateMode::Unimplemented => note_first(&mut unimplemented, s.to_owned()),
                DateMode::Unknown => {
                    eprintln!("fatal: unknown date format {}", &s["--date=".len()..]);
                    return Ok(128);
                }
            },
            "--relative-date" => note_first(&mut unimplemented, a.to_owned()),

            // ---- reflog-entry date limiters -------------------------------
            // git filters `-g` on the reflog entry's own timestamp (set from the
            // fake reflog parent), not the commit date. `--since`/`--after` keep
            // entries at or after the instant, `--until`/`--before` at or before.
            s if s.starts_with("--since=") || s.starts_with("--after=") => {
                let v = s.split_once('=').expect("checked for `=` above").1;
                opts.since = Some(parse_limit_date(v));
            }
            s if s.starts_with("--until=") || s.starts_with("--before=") => {
                let v = s.split_once('=').expect("checked for `=` above").1;
                opts.until = Some(parse_limit_date(v));
            }

            // ---- decoration -----------------------------------------------
            "--decorate" | "--decorate=short" => opts.decorate = Some(Decorate::Short),
            "--decorate=full" => opts.decorate = Some(Decorate::Full),
            // `auto` decorates only on a tty; the parity harness pipes, so it is off,
            // as are the explicit off spellings.
            "--decorate=no" | "--no-decorate" | "--decorate=auto" => opts.decorate = None,

            // ---- output format --------------------------------------------
            // Bare `--pretty` is git's shorthand for `--pretty=medium`; bare
            // `--format` (no `=`) is not an option at all — git reports it as an
            // unrecognized argument, so it falls through to that arm below.
            "--oneline" => opts.out = OutFmt::Oneline,
            "--pretty" => opts.out = OutFmt::Builtin(Builtin::Medium),
            s if s.starts_with("--pretty=") || s.starts_with("--format=") => {
                let v = s.split_once('=').expect("checked for `=` above").1;
                match classify_pretty(repo, v) {
                    Pretty::Oneline => opts.out = OutFmt::Oneline,
                    Pretty::Builtin(b) => opts.out = OutFmt::Builtin(b),
                    Pretty::Custom(f) => match unsupported_placeholder(&f) {
                        Some(p) => {
                            note_first(&mut unimplemented, format!("{s} (placeholder {p})"));
                        }
                        None => opts.out = OutFmt::Custom(f),
                    },
                    Pretty::Unimplemented => note_first(&mut unimplemented, s.to_owned()),
                    Pretty::Invalid => {
                        eprintln!("fatal: invalid --pretty format: {v}");
                        return Ok(128);
                    }
                    // A `pretty.<name>` alias chain that loops names the format
                    // rather than the option value (pretty.c:156-158).
                    Pretty::Cycle(msg) => {
                        eprintln!("fatal: {msg}");
                        return Ok(128);
                    }
                }
            }

            // ---- ref sets --------------------------------------------------
            "--all" => {
                saw_ref_source = true;
                sections.extend(expand_all(repo, &excludes)?);
            }
            "--branches" | "--tags" | "--remotes" => {
                saw_ref_source = true;
                sections.extend(expand_prefixed(repo, ref_prefix(a), None, &excludes)?);
            }
            s if s.starts_with("--branches=")
                || s.starts_with("--tags=")
                || s.starts_with("--remotes=") =>
            {
                saw_ref_source = true;
                let (key, pat) = s.split_once('=').expect("checked for `=` above");
                sections.extend(expand_prefixed(repo, ref_prefix(key), Some(pat), &excludes)?);
            }
            s if s.starts_with("--glob=") => {
                saw_ref_source = true;
                sections.extend(expand_glob(repo, &s["--glob=".len()..], &excludes)?);
            }
            s if s.starts_with("--exclude=") => excludes.push(s["--exclude=".len()..].to_owned()),

            // ---- filtering / extra columns ---------------------------------
            "--merges" => opts.merges = Some(true),
            "--no-merges" => opts.merges = Some(false),
            "--parents" => opts.parents = true,

            // ---- post-scan conflicts ---------------------------------------
            "--graph" | "--children" | "--topo-order" | "--date-order"
            | "--author-date-order" => limited = true,
            "--reverse" => reverse = true,

            // ---- diff output ------------------------------------------------
            "--name-only" => opts.diff.name_only = true,
            "--name-status" => opts.diff.name_status = true,
            "--numstat" => opts.diff.numstat = true,
            "--shortstat" => opts.diff.shortstat = true,
            "--summary" => opts.diff.summary = true,
            "--raw" => opts.diff.raw = true,
            // git assigns `DIFF_FORMAT_NO_OUTPUT` here rather than or-ing a bit, so
            // this drops every diff format named to its left.
            "--no-patch" | "-s" => opts.diff.set_no_output(),

            // ---- message filtering -----------------------------------------
            // git applies `--grep` to each entry's commit message. The dialect and
            // case/invert/all-match modifiers are collected here and compiled once
            // after the scan, matching git's `setup_revisions` ordering.
            s if s.starts_with("--grep=") => {
                grep_patterns.push(s["--grep=".len()..].to_owned());
            }
            s if s.starts_with("--grep-reflog=") => {
                grep_reflog_patterns.push(s["--grep-reflog=".len()..].to_owned());
            }
            "--invert-grep" => grep_invert = true,
            "--all-match" => grep_all_match = true,
            "--regexp-ignore-case" | "-i" => grep_ignore_case = true,
            "--fixed-strings" | "-F" => grep_kind = GrepKind::Fixed,
            "--basic-regexp" => grep_kind = GrepKind::Basic,
            "--extended-regexp" | "-E" | "--perl-regexp" | "-P" => {
                grep_kind = GrepKind::Extended;
            }

            // ---- recognized, no effect on reflog output ---------------------
            // Each of these was verified byte-identical to plain `git reflog`.
            "--first-parent" => opts.first_parent = true,
            "--walk-reflogs" | "-g" | "--single-worktree" | "--boundary"
            | "--source" | "--no-color" => {}

            // `--color[=<when>]` is `OPT__COLOR`, whose `parse_opt_color_flag_cb()`
            // (`parse-options-cb.c:50`) calls `git_config_colorbool(NULL, arg)`. With
            // no variable name to fall back on, that accepts only `always`, `auto`
            // and `never` (case-insensitively) — boolean spellings such as `true` are
            // config values, not `--color` values — and anything else is the
            // callback's `error()` followed by parse-options' exit 129. That is an
            // immediate exit at this argument's position, so it beats every check
            // deferred to the end of the scan, including the revision resolution the
            // remaining arguments would otherwise reach.
            s if s == "--color" || s.starts_with("--color=") => {
                let when = s.strip_prefix("--color=");
                match when {
                    // A missing value is the option's `defval`, `always`.
                    None => note_first(&mut unimplemented, a.to_owned()),
                    Some(v) if v.eq_ignore_ascii_case("always") => {
                        note_first(&mut unimplemented, a.to_owned());
                    }
                    // Both are "no color" for a non-terminal stdout, which is what
                    // this renderer already produces.
                    Some(v)
                        if v.eq_ignore_ascii_case("never") || v.eq_ignore_ascii_case("auto") => {}
                    Some(_) => {
                        eprintln!(
                            "error: option `color' expects \"always\", \"auto\", or \"never\""
                        );
                        return Ok(129);
                    }
                }
            }

            // ---- recognized, deliberately unimplemented ---------------------
            // `log -p`'s renderer is shared, so the patch body is real output
            // rather than a refusal — `git stash list -p` is `log -g -p`.
            "-p" | "--patch" | "-u" => opts.diff.patch = true,
            "--stat" => opts.diff.stat = true,
            // `--patch-with-stat` is `--stat -p`, which is how git renders it.
            "--patch-with-stat" => {
                opts.diff.stat = true;
                opts.diff.patch = true;
            }
            "--dirstat" => {
                note_first(&mut unimplemented, a.to_owned());
            }
            // `--stat=<width>[,<name-width>[,<count>]]` needs `show_stats()`'s
            // width derivation — total line width against the terminal's, with
            // the graph taking what the name column leaves — which no renderer
            // here implements (`diff --stat=<width>` drops the parameters too).
            // Refused rather than rendered at the default width.
            s if s.starts_with("--stat=") || s.starts_with("--dirstat=") => {
                note_first(&mut unimplemented, s.to_owned());
            }

            // ---- unknown option --------------------------------------------
            s if s.starts_with('-') => {
                if unrecognized.is_none() {
                    unrecognized = Some(s.to_owned());
                }
            }

            // ---- revision ---------------------------------------------------
            s => {
                // `handle_revision_arg_1()`'s very first test:
                //
                // ```c
                // if (!cant_be_filename && !strcmp(arg, "..")) {
                //         /*
                //          * Just ".."?  That is not a range but the
                //          * pathspec for the parent directory.
                //          */
                //         return -1;
                // }
                // ```
                //
                // (revision.c:2164). The `-1` sends `setup_revisions()` down its
                // filename fallback, which checks this operand and every one
                // after it and then makes prune data of the lot:
                //
                // ```c
                // for (j = i; j < argc; j++)
                //         verify_filename(revs->prefix, argv[j], j == i);
                // strvec_pushv(&prune_data, argv + i);
                // break;
                // ```
                //
                // (revision.c:2907-2911) — so `git reflog show ..` ends at the
                // pathspec layer's `'..' is outside repository`, and
                // `git reflog show .. nosuchfile` at the second operand instead.
                if crate::objname::is_parent_directory_pathspec(s, seen_dashdash) {
                    for (n, arg) in rest[i..].iter().enumerate() {
                        if let Some(msg) = crate::setup::verify_filename(arg, n == 0) {
                            eprintln!("fatal: {msg}");
                            return Ok(128);
                        }
                    }
                    opts.pathspecs = rest[i..].iter().map(|s| s.as_bytes().to_vec()).collect();
                    break;
                }
                saw_ref_source = true;
                // One operand can contribute more than one section: `<rev>^@`
                // queues every parent under the base's name, so a merge's `^@`
                // walks that reflog once per parent, as stock 2.55.0 does.
                if let Some(code) = resolve_operand(repo, s, &mut sections)? {
                    return Ok(code);
                }
            }
        }
        i += 1;
    }

    // git's `diff_setup_done()` rejects more than one of these before any of the
    // revision-walk conflicts below, and before the unrecognized-argument report.
    if opts.diff.exclusive_bits() > 1 {
        eprintln!(
            "fatal: options '--name-only', '--name-status', '--check', and '-s' \
             cannot be used together"
        );
        return Ok(128);
    }
    opts.diff.resolve();

    if limited {
        eprintln!("fatal: cannot combine --walk-reflogs with history-limiting options");
        return Ok(128);
    }
    if reverse {
        eprintln!("fatal: options '--reverse' and '--walk-reflogs' cannot be used together");
        return Ok(128);
    }
    if let Some(arg) = unrecognized {
        eprintln!("fatal: unrecognized argument: {arg}");
        return Ok(128);
    }

    // git compiles `--grep` patterns once the whole command line is parsed; a bad
    // pattern is fatal (exit 128), as it is in git's `compile_regexp`.
    if !grep_patterns.is_empty() || !grep_reflog_patterns.is_empty() {
        // git names the origin of a bad pattern: `command line` for a message
        // grep, `header` for the `reflog` header grep `--grep-reflog` adds.
        let compile = |pats: &[String], origin: &str| -> std::result::Result<Vec<Regex>, u8> {
            let mut compiled: Vec<Regex> = Vec::with_capacity(pats.len());
            for pat in pats {
                let translated = match grep_kind {
                    GrepKind::Fixed => regex::escape(pat),
                    GrepKind::Extended => pat.clone(),
                    GrepKind::Basic => bre_to_ere(pat),
                };
                match RegexBuilder::new(&translated)
                    .case_insensitive(grep_ignore_case)
                    .multi_line(true)
                    .build()
                {
                    Ok(re) => compiled.push(re),
                    Err(_) => {
                        eprintln!("fatal: {origin}, '{pat}': invalid regular expression");
                        return Err(128);
                    }
                }
            }
            Ok(compiled)
        };
        let patterns = match compile(&grep_patterns, "command line") {
            Ok(v) => v,
            Err(code) => return Ok(code),
        };
        let reflog_patterns = match compile(&grep_reflog_patterns, "header") {
            Ok(v) => v,
            Err(code) => return Ok(code),
        };
        opts.grep = Some(GrepFilter {
            patterns,
            reflog_patterns,
            all_match: grep_all_match,
            invert: grep_invert,
        });
    }

    // Bare `git reflog` on an unborn HEAD has its own fatal message in git,
    // distinct from the "ambiguous argument" one an explicit `HEAD` produces.
    if !saw_ref_source {
        if let Ok(head) = repo.head() {
            if head.is_unborn() {
                let branch = head
                    .referent_name()
                    .map(|n| n.shorten().to_str_lossy().into_owned())
                    .unwrap_or_else(|| "master".to_owned());
                eprintln!("fatal: your current branch '{branch}' does not have any commits yet");
                return Ok(128);
            }
        }
        match resolve_spec(repo, "HEAD")? {
            Resolved::Section(section) => sections.push(section),
            Resolved::Empty => {}
            Resolved::Fatal(code) => return Ok(code),
        }
    }

    render(repo, &sections, &opts, full_hex, &unimplemented, tweak)
}

/// Walk the collected sections and write git's output for them.
///
/// `unimplemented` names the first option that this module recognizes but cannot
/// render. It is deferred to here rather than failing during the argument scan
/// because a filter (a date limiter or a pathspec) may drop every entry, in which
/// case git prints nothing and the unsupported option never comes into play — so
/// the failure is raised only when an entry actually survives every filter and
/// therefore would be printed.
fn render(
    repo: &gix::Repository,
    sections: &[Section],
    opts: &Opts,
    full_hex: usize,
    unimplemented: &Option<String>,
    tweak: Tweak,
) -> Result<u8> {
    let fallback_len = abbrev_len(repo, full_hex);
    // `diff_merges_default_to_first_parent()`, which only `git log`'s tweak hook
    // calls. Without it a merge entry has no diff in any format.
    let first_parent_merges = opts.first_parent && tweak == Tweak::Log;
    // The field date format (`%ad`/`%cd`, the `Date:` header lines): an explicit
    // `--date=` wins, then `log.date`, then git's default layout.
    let field_fmt: DateFormat = opts
        .date
        .or(opts.log_date)
        .unwrap_or_else(|| DateFormat::plain(tfmt::DEFAULT));

    let mut skipped = 0usize;
    let mut printed = 0usize;
    let budget = opts.max_count.unwrap_or(usize::MAX);
    let mut out: Vec<u8> = Vec::new();
    // Built once and reused: it caches decoded blobs across every entry's diff.
    // Needed for the diff formats and for pathspec filtering, both of which walk
    // each entry's tree diff.
    let mut diff_cache = (opts.diff.any() || !opts.pathspecs.is_empty())
        .then(|| repo.diff_resource_cache_for_tree_diff().ok())
        .flatten();
    // The `--` pathspec set, parsed once for the whole listing.
    let mut path_specs = if opts.pathspecs.is_empty() {
        None
    } else {
        Some(super::log::PathspecMatcher::new(repo, &opts.pathspecs)?)
    };
    // The ref-set that decorates each entry's commit, resolved once. `--decorate`
    // fixes the mode; a `%d`/`%D` in a user format needs decorations too and, with
    // no `--decorate`, git defaults it to the short form.
    let deco_mode = opts.decorate.or_else(|| {
        matches!(&opts.out, OutFmt::Custom(f) if format_uses_decoration(f)).then_some(Decorate::Short)
    });
    let decorations = deco_mode.map(|mode| Decorations::build(repo, mode));

    'outer: for section in sections {
        // `reflog-walk.c:get_reflog_selector`: dates for a `@{<date>}` argument,
        // or for an argument with no selector at all when `--date=` forced them.
        // An `@{<n>}` argument counts regardless.
        let selector_fmt: Option<DateFormat> = match section.selector {
            SelectorKind::Date => Some(opts.date.unwrap_or_else(|| DateFormat::plain(tfmt::DEFAULT))),
            SelectorKind::None => opts.date,
            SelectorKind::Index => None,
        };
        for (n, entry) in section.entries.iter().enumerate().skip(section.start) {
            // `git reflog` is `git log --walk-reflogs`, and that walk hands out
            // *commits*, not reflog lines. `next_reflog_entry()` only ever returns
            // what `next_reflog_commit()` found, and that function steps the cursor
            // past every entry whose **new** object is not a commit:
            //
            // ```c
            // for (; log->recno >= 0; log->recno--) {
            //         struct reflog_info *entry = &log->reflogs->items[log->recno];
            //         struct object *obj = parse_object(the_repository,
            //                                           &entry->noid);
            //
            //         if (obj && obj->type == OBJ_COMMIT)
            //                 return (struct commit *)obj;
            // }
            // return NULL;
            // ```
            // (`reflog-walk.c:341-352`)
            //
            // The test is `parse_object()` **plus** `type == OBJ_COMMIT`, not "the
            // id is null". Three unrelated shapes fail it for the same reason: the
            // zero id a deletion records (nothing to parse), an id whose object the
            // repository no longer holds (pruned, or a shallow/partial clone), and
            // an id that parses to the wrong type because the ref pointed at a tag,
            // a tree or a blob. `parse_object` does not peel, so an annotated tag is
            // dropped as surely as a missing object.
            //
            // `branch -m` is the everyday source of these: a rename logs the old
            // name's *deletion* into HEAD's log (`<commit> -> 0{40}`) next to the new
            // name's creation, so every rename leaves one unwalkable entry behind.
            //
            // A dropped entry still spends its `@{…}` number. The selector is
            // computed from the array slot the survivor occupies —
            // `strbuf_addf(sb, "%d", commit_reflog->reflogs->nr - 2 - commit_reflog->recno)`
            // (`reflog-walk.c:266-267`), against a `recno` that `next_reflog_entry()`
            // already decremented past the returned entry (`reflog-walk.c:379`), so
            // it reads back as `nr - 1 - <array index>`. Two renames therefore print
            // `@{0}`, `@{2}`, `@{4}` rather than a renumbered `@{0}`, `@{1}`, `@{2}`.
            // That is why this is a `continue` that leaves `n` untouched, and not a
            // filtering pass over `section.entries`.
            if !matches!(
                repo.find_header(entry.oid).map(|h| h.kind()),
                Ok(gix::objs::Kind::Commit)
            ) {
                continue;
            }
            if let Some(want_merge) = opts.merges {
                if is_merge(repo, entry.oid) != want_merge {
                    continue;
                }
            }
            // git's `-g` date limiting compares against the reflog entry's own
            // timestamp, not the commit date.
            if opts.since.is_some_and(|s| entry.time.seconds < s) {
                continue;
            }
            if opts.until.is_some_and(|u| entry.time.seconds > u) {
                continue;
            }
            // git's `--grep` limits the walk to entries whose commit message
            // matches, before the diff of the entry is ever computed.
            if let Some(grep) = &opts.grep {
                let message = repo
                    .find_commit(entry.oid)
                    .ok()
                    .and_then(|c| c.message_raw().ok().map(|m| m.to_vec()))
                    .unwrap_or_default();
                if !grep.keeps(&message, &entry.message) {
                    continue;
                }
            }
            // git diffs each entry's commit against its first parent, whatever the
            // reflog message says the entry was. Computed before the skip/count
            // budget because pathspec filtering must run first.
            let changes = match diff_cache.as_mut() {
                Some(cache) => collect_changes(repo, entry.oid, cache, first_parent_merges),
                None => Vec::new(),
            };
            // Pathspecs keep only entries whose diff touches the set.
            if let Some(specs) = path_specs.as_mut() {
                if !pathspec_matches(&changes, specs) {
                    continue;
                }
            }
            if skipped < opts.skip {
                skipped += 1;
                continue;
            }
            if printed >= budget {
                break 'outer;
            }
            // This entry survived every filter, so git would print it. If some
            // option was recognized but this module cannot render it, faithful
            // output is impossible now — fail rather than print a wrong answer.
            if let Some(what) = unimplemented {
                bail!("`reflog show {what}` is not ported");
            }
            // An unrenderable `log.date` mode only matters once an entry is about
            // to print in a format that renders a field date, and only when no
            // `--date=` overrode it (git validated the value itself at startup).
            if let Some(value) = &opts.log_date_unsupported {
                if opts.date.is_none() && renders_field_date(&opts.out) {
                    bail!(
                        "`reflog show` with log.date={value} is not ported: it needs the \
                         current time or a strftime user format, which gix-date does not expose"
                    );
                }
            }
            let selector = match selector_fmt {
                Some(f) => f.render(entry.time),
                None => n.to_string(),
            };
            match &opts.out {
                OutFmt::Custom(fmt) => {
                    let line = expand_format(
                        repo,
                        fmt,
                        section,
                        entry,
                        &selector,
                        opts,
                        field_fmt,
                        fallback_len,
                        decorations.as_ref(),
                    );
                    // git emits a line per entry whenever the format STRING is
                    // non-empty — even when it expands to nothing (e.g. `%D` on a
                    // commit with no refs prints a blank line). An empty format
                    // string (`--pretty=`) prints nothing at all.
                    if !fmt.is_empty() {
                        out.extend_from_slice(&line);
                        out.push(b'\n');
                        // A user format is separated from the diff by a blank line,
                        // emitted whenever the diff queue is non-empty — even when
                        // the selected format renders none of those changes. With a
                        // stat *and* a patch, git's `log --stat -p` layout puts
                        // `---` there instead. The separator belongs to the diff, so
                        // it needs a diff format to have been asked for: a pathspec
                        // alone builds the change list to filter with and prints no
                        // diff, and so no blank line either.
                        if opts.diff.any() && !changes.is_empty() {
                            if opts.diff.stat && opts.diff.wants_patch() {
                                out.extend_from_slice(b"---\n");
                            } else {
                                out.push(b'\n');
                            }
                        }
                    }
                }
                OutFmt::Oneline => {
                    out.extend_from_slice(
                        abbrev_id(repo, entry.oid, &opts.abbrev, fallback_len).as_bytes(),
                    );
                    if opts.parents {
                        for parent in parents_of(repo, entry.oid) {
                            out.push(b' ');
                            out.extend_from_slice(
                                abbrev_id(repo, parent, &opts.abbrev, fallback_len).as_bytes(),
                            );
                        }
                    }
                    // git's `--decorate` annotates the commit right after its id.
                    if let Some(deco) = &decorations {
                        if let Some(text) = deco.for_commit(entry.oid) {
                            out.push(b' ');
                            out.extend_from_slice(text.as_bytes());
                        }
                    }
                    out.push(b' ');
                    out.extend_from_slice(section.display.as_bytes());
                    out.extend_from_slice(format!("@{{{selector}}}: ").as_bytes());
                    out.extend_from_slice(&entry.message);
                    out.push(b'\n');
                }
                OutFmt::Builtin(kind) => {
                    // The header formats put a blank line between consecutive
                    // entries; the first printed entry gets none.
                    if kind.separates() && printed > 0 {
                        out.push(b'\n');
                    }
                    let block = build_builtin_block(
                        repo,
                        *kind,
                        section,
                        entry,
                        &selector,
                        opts,
                        field_fmt,
                        fallback_len,
                        decorations.as_ref(),
                    );
                    out.extend_from_slice(&block);
                    // A diff, when one is selected, is separated by a blank line —
                    // except when a stat *and* a patch are both printed, where
                    // git's `log --stat -p` layout separates them with `---`.
                    if !changes.is_empty() {
                        if opts.diff.stat && opts.diff.wants_patch() {
                            out.extend_from_slice(b"---\n");
                        } else {
                            out.push(b'\n');
                        }
                    }
                }
            }
            append_diff(
                &mut out,
                repo,
                &changes,
                opts.diff,
                &opts.abbrev,
                fallback_len,
            );
            if opts.diff.wants_patch() {
                // The stat block and the patch are separated by a blank line.
                if opts.diff.stat && !changes.is_empty() {
                    out.push(b'\n');
                }
                if let Ok(commit) = repo.find_commit(entry.oid) {
                    // A merge is diffed only where git's `log_setup_revisions_tweak`
                    // ran, i.e. under `git log -g --first-parent`. `git reflog show`
                    // installs no tweak, so it prints the entry lines alone however
                    // it was formatted and whether or not `--first-parent` was given.
                    let merge = commit.parent_ids().count() > 1;
                    if merge && !first_parent_merges {
                        printed += 1;
                        continue;
                    }
                    let parent = commit.parent_ids().next().map(|id| id.detach());
                    if let Ok(patch) = super::diff::commit_patch(repo, &commit, parent, 3) {
                        out.extend_from_slice(&patch);
                    }
                }
            }
            printed += 1;
        }
    }

    std::io::stdout().write_all(&out)?;
    Ok(0)
}

/// The outcome of resolving one non-option argument.
enum Resolved {
    Section(Section),
    Empty,
    Fatal(u8),
}

/// `add_reflog_for_walk()`'s refusal (`reflog-walk.c`):
///
/// ```c
/// if (commit->object.flags & UNINTERESTING)
///         die("cannot walk reflogs for %s", name);
/// ```
///
/// `name` is `add_pending_object()`'s, i.e. the operand with its `^` and any
/// `^@`/`^!`/`^-<n>` mark already off — which is why `git reflog show HEAD^!` is
/// `fatal: cannot walk reflogs for HEAD` and not `… for HEAD^!`.
fn cannot_walk(name: &str) -> u8 {
    eprintln!("fatal: cannot walk reflogs for {name}");
    128
}

/// One whole `git reflog show` operand, in `handle_revision_arg_1()`'s order:
/// the parent marks, then the exclusion `^`, then the name.
///
/// Sections are appended rather than returned because one operand can produce
/// several: `add_parents_only()` calls `add_pending_object()` once per parent,
/// and with `revs->reflog_info` set each of those calls is an
/// `add_reflog_for_walk()` for the *base's* name — so `<merge>^@` walks that
/// reflog twice, which stock 2.55.0 duly prints twice.
///
/// `Some(code)` is a fatal; `None` means whatever this operand contributed is
/// already in `sections`, which for a name that owns no reflog is nothing.
fn resolve_operand(
    repo: &gix::Repository,
    spec: &str,
    sections: &mut Vec<Section>,
) -> Result<Option<u8>> {
    // ---- handle_dotdot() ---------------------------------------------------
    //
    // It runs ahead of the three-mark block below and is the *whole* of the
    // range rule: both endpoints through `get_oid_with_context()`,
    // `parse_object()` on each, and — for `<a>...<b>` only —
    // `lookup_commit_reference()` on each. Asked of [`crate::objname`] rather
    // than re-derived here, the same way `format-patch` and `bundle` ask it.
    //
    // The bare-`..` guard in front of it belongs to the caller: it needs the
    // rest of argv, because that operand and every one after it become prune
    // data.
    let range = crate::objname::split_range(spec).map(|r| {
        // The `warning: refname … is ambiguous.` half of those two
        // `get_oid_with_context()` calls. [`crate::objname::dotdot`] is a quiet
        // classifier, so the warning is asked for separately and exactly once —
        // and [`dotdot_walks`] below silences the by-name resolutions it does,
        // which git does not repeat.
        crate::objname::warn_dotdot_endpoints(repo, spec);
        (r, crate::objname::dotdot(repo, spec))
    });
    if let Some((r, crate::objname::Dotdot::Missing { .. })) = &range {
        // `dotdot_missing()`, with whatever `lookup_commit_reference()` printed
        // ahead of it.
        eprint!(
            "{}",
            crate::objname::dotdot_fatal(repo, spec).unwrap_or_else(|| format!(
                "fatal: {}\n",
                crate::objname::dotdot_missing_message(spec, r.symmetric)
            ))
        );
        return Ok(Some(128));
    }
    if let Some((r, crate::objname::Dotdot::Ok { a, b })) = range {
        return dotdot_walks(repo, &r, a, b, sections);
    }

    // ---- handle_revision_arg_1()'s three-mark block -----------------------
    //
    // `git reflog show` reaches `setup_revisions()` through `cmd_log_reflog()`,
    // so it reads the same grammar as every other revision-taking verb. The
    // marks are `handle_revision_arg_1()`'s own and `get_oid_1()` has no case
    // for them, which is why an operand that keeps one cannot resolve at all.
    // See [`crate::objname::parents_only`] for the C.
    let spec: &str = match crate::objname::parents_only(spec) {
        // No mark, or a `^-<n>` whose number git refused outright — both leave
        // the operand exactly as typed.
        crate::objname::ParentsOnly::Absent | crate::objname::ParentsOnly::BadParent => spec,
        crate::objname::ParentsOnly::Mark { base, nth, replaces } => {
            // `^@` queues the parents under `flags`, which here is "interesting";
            // `^!` and `^-<n>` under `flags ^ (UNINTERESTING | BOTTOM)`, and an
            // UNINTERESTING commit is exactly what `add_reflog_for_walk()`
            // refuses — so `<rev>^!` is a fatal wherever `<rev>` has a parent.
            let mut uninteresting_parent = None;
            let mut walks: Vec<String> = Vec::new();
            let mut queue = |name: &str, _parent, uninteresting: bool| {
                if uninteresting {
                    uninteresting_parent = Some(name.to_owned());
                } else {
                    walks.push(name.to_owned());
                }
            };
            let queued =
                crate::objname::add_parents_only(repo, base, !replaces, nth, &mut queue);
            if let Some(name) = uninteresting_parent {
                return Ok(Some(cannot_walk(&name)));
            }
            match queued {
                // `get_reference()`'s `die(_("bad object %s"), name)`.
                crate::objname::Parents::BadObject => {
                    let name = crate::objname::uninteresting_mark(base).0;
                    eprintln!("fatal: bad object {name}");
                    return Ok(Some(128));
                }
                // `arg` untouched: the operand carries its mark into a
                // resolution that cannot succeed, and is diagnosed there.
                crate::objname::Parents::None => spec,
                crate::objname::Parents::Queued => {
                    // `add_reflog_for_walk()` reads the log by *name*; it never
                    // resolves anything, so these walks add no ambiguity
                    // warning of their own. The one git prints for the operand
                    // was already printed by `add_parents_only()` above.
                    let _quiet = crate::objname::AmbiguityWarnings::off();
                    for name in &walks {
                        match resolve_spec(repo, name)? {
                            Resolved::Section(section) => sections.push(section),
                            Resolved::Empty => {}
                            Resolved::Fatal(code) => return Ok(Some(code)),
                        }
                    }
                    // `^@` returns from `handle_revision_arg_1()`; `^!` and
                    // `^-<n>` go on to pend the base as well — but they only get
                    // here when the commit had no parent at all, since any
                    // parent they queued is UNINTERESTING and already died.
                    if replaces {
                        return Ok(None);
                    }
                    base
                }
            }
        }
    };

    // ---- the exclusion `^` -------------------------------------------------
    //
    // `if (*arg == '^') { flags ^= UNINTERESTING | BOTTOM; arg++; }`, and
    // `add_pending_object_with_path()` only takes the reflog branch for an
    // `OBJ_COMMIT`. So `^<commit>` is the refusal above while `^<tag>` and
    // `^<tree>` are silent — the tag is pended unpeeled and simply dropped.
    // `get_oid_with_context()` sees the stripped name, so the operand is
    // resolved — and warned about — exactly once here, as it is below.
    if let Some(name) = spec.strip_prefix('^') {
        let Some(id) = crate::objname::resolve(repo, name) else {
            // `setup_revisions()`: `if (seen_dashdash || *arg == '^') die(_("bad
            // revision '%s'"), arg);` — quoting the operand as typed, caret and
            // all, rather than reaching the pathspec fallback.
            eprintln!("fatal: bad revision '{spec}'");
            return Ok(Some(128));
        };
        // `get_reference()`'s `die(_("bad object %s"), name)` for a full-length
        // hex the repository does not have — the caret is already off.
        let Ok(object) = repo.find_object(id) else {
            eprintln!("fatal: bad object {name}");
            return Ok(Some(128));
        };
        if object.kind == gix::object::Kind::Commit {
            return Ok(Some(cannot_walk(name)));
        }
        // A tag, tree or blob never reaches `add_reflog_for_walk()`: it goes to
        // `revs->pending` and is dropped by the walk, silently and with exit 0.
        return Ok(None);
    }

    Ok(match resolve_spec(repo, spec)? {
        Resolved::Section(section) => {
            sections.push(section);
            None
        }
        Resolved::Empty => None,
        Resolved::Fatal(code) => Some(code),
    })
}

/// What a range operand contributes once `handle_dotdot_1()` has resolved it.
///
/// The whole of the difference from every other verb is which pending entries
/// come out UNINTERESTING, because `add_reflog_for_walk()` refuses exactly those:
///
/// ```c
/// if (!symmetric) {
///         /* just A..B */
///         b_flags = flags;
///         a_flags = flags_exclude;
/// } else {
///         /* A...B -- find merge bases between the two */
///         exclude = get_merge_bases(a, b);
///         add_rev_cmdline_list(revs, exclude, REV_CMD_MERGE_BASE, flags_exclude);
///         add_pending_commit_list(revs, exclude, flags_exclude);
///         b_flags = flags;
///         a_flags = flags | SYMMETRIC_LEFT;
/// }
/// a_obj->flags |= a_flags;
/// b_obj->flags |= b_flags;
/// add_pending_object_with_path(revs, a_obj, a_name, …);
/// add_pending_object_with_path(revs, b_obj, b_name, …);
/// ```
///
/// (revision.c). So `<a>..<b>` excludes its *left* end and dies naming it, while
/// `<a>...<b>` excludes only the merge bases — which
/// `add_pending_commit_list()` pends under `oid_to_hex()`, which is why stock
/// 2.55.0 reports a 40-hex there and not either endpoint. The bases are pended
/// first, so they win the refusal even when the left end is a commit too.
///
/// Both ends of a symmetric difference stay interesting, and that is not a dead
/// branch: two histories with no merge base at all — `git reflog other...side`
/// across two roots — walk *both* reflogs and exit 0.
///
/// `add_pending_object_with_path()` only takes the reflog branch for an
/// `OBJ_COMMIT`, so an endpoint that is an annotated tag is pended and dropped
/// instead: it neither dies nor walks.
fn dotdot_walks(
    repo: &gix::Repository,
    r: &crate::objname::Range<'_>,
    a: gix::hash::ObjectId,
    b: gix::hash::ObjectId,
    sections: &mut Vec<Section>,
) -> Result<Option<u8>> {
    // `add_reflog_for_walk()` reads its log by *name* and resolves nothing, so
    // none of the lookups below warn. The one warning git prints for the operand
    // came from `handle_dotdot_1()`'s two `get_oid_with_context()` calls, which
    // the caller has already made.
    let _quiet = crate::objname::AmbiguityWarnings::off();
    let mut walks: Vec<&str> = Vec::new();
    if r.symmetric {
        // `a` and `b` are `lookup_commit_reference()`'s output here, which is
        // what `get_merge_bases()` is handed.
        if let Some(base) = repo.merge_bases_many(a, &[b])?.first() {
            return Ok(Some(cannot_walk(&base.detach().to_string())));
        }
        walks.push(r.a);
        walks.push(r.b);
    } else {
        // The left end carries `flags_exclude`. Only a commit reaches
        // `add_reflog_for_walk()` at all, and reaching it with UNINTERESTING set
        // is the refusal.
        if is_commit(repo, r.a) {
            return Ok(Some(cannot_walk(r.a)));
        }
        walks.push(r.b);
    }
    for name in walks {
        // A non-commit endpoint never registers a walk; it goes to
        // `revs->pending` and is dropped there.
        if !is_commit(repo, name) {
            continue;
        }
        match resolve_spec(repo, name)? {
            Resolved::Section(section) => sections.push(section),
            Resolved::Empty => {}
            Resolved::Fatal(code) => return Ok(Some(code)),
        }
    }
    Ok(None)
}

/// Whether the object `name` resolves to is an `OBJ_COMMIT` — the one test
/// `add_pending_object_with_path()` makes before it hands an entry to
/// `add_reflog_for_walk()`. The object is the one `parse_object()` returned, so
/// an annotated tag answers no even though it peels to a commit.
fn is_commit(repo: &gix::Repository, name: &str) -> bool {
    crate::objname::resolve_quiet(repo, name)
        .and_then(|id| repo.find_object(id).ok())
        .is_some_and(|object| object.kind == gix::object::Kind::Commit)
}

/// Resolve a `<ref>`, `<ref>@{<n>}` or `<ref>@{<date>}` argument the way git's
/// revision parser does, reporting git's own fatal text at the failure points.
fn resolve_spec(repo: &gix::Repository, spec: &str) -> Result<Resolved> {
    // `cmd_log_reflog()` hands every operand to `setup_revisions()`, so
    // `get_oid_basic()` sees it before the reflog is ever opened — and a
    // full-length hex takes that function's *first* branch, warning about a
    // same-named ref and returning the id. The walk still shows that ref's log,
    // because `add_reflog_for_walk()` dwims `e->name` rather than the id, which
    // is why the warning is all that distinguishes the two implementations here.
    //
    // Once per operand: this function is called once for each argument and once
    // for the `HEAD` default, exactly as git resolves them.
    //
    // The exclusion mark is split off first because `handle_revision_arg_1()`
    // advances past it (`if (*arg == '^') { … arg++; }`) before
    // `get_oid_with_context()` is called, so `get_oid_basic()` measures the name
    // *without* the caret and its full-hex branch is the one taken —
    // `git reflog show ^<40-hex-ref>` warns in stock 2.55.0. `repo_get_oid()`
    // does no such strip, which is why [`crate::objname::ambiguity_base`] does
    // not either and the walkers do it here.
    crate::objname::warn_ambiguous_refname(repo, crate::objname::uninteresting_mark(spec).0);

    // `repo_interpret_branch_name()` rewrites the whole operand before either
    // `get_oid_basic()` or `add_reflog_for_walk()` sees it, so `git reflog show @`
    // reads HEAD's log and `git reflog show <branch>@{u}` reads the upstream's.
    let rewritten;
    let spec: &str = match crate::objname::interpret_branch_name(repo, spec) {
        Some(Ok(name)) => {
            rewritten = name;
            rewritten.as_str()
        }
        Some(Err(message)) => {
            eprintln!("fatal: {message}");
            return Ok(Resolved::Fatal(128));
        }
        None => spec,
    };

    let (base, selector) = split_selector(spec);

    // `get_oid_basic()` resolves the operand before `add_reflog_for_walk()` ever
    // opens a log, and which lookup it uses depends on whether a selector was
    // typed:
    //
    // ```c
    // if (!len && reflog_len)
    //         /* allow "@{...}" to mean the current branch reflog */
    //         refs_found = repo_dwim_ref(r, "HEAD", 4, oid, &real_ref, !fatal);
    // else if (reflog_len)
    //         refs_found = repo_dwim_log(r, str, len, oid, &real_ref);
    // else
    //         refs_found = repo_dwim_ref(r, str, len, oid, &real_ref, !fatal);
    // if (!refs_found)
    //         return -1;
    // ```
    // (`object-name.c:742-751`)
    //
    // Both dwims insist the name resolves *to an object*, which is what makes a
    // stale `logs/HEAD` under an unborn HEAD a fatal rather than a listing: the
    // log file is there, but `HEAD` names no commit.
    match selector {
        Some(_) if base.is_empty() => {
            if crate::refname::resolve_ref_reading(repo, "HEAD").is_none() {
                return Ok(Resolved::Fatal(fatal_ambiguous(spec)));
            }
        }
        Some(_) => {
            if dwim_log(repo, base).is_none() {
                return Ok(Resolved::Fatal(fatal_ambiguous(spec)));
            }
        }
        None => {
            // `get_oid_with_context()` is the full-hex rule — an id the repository
            // does not have still resolves, without the object database being
            // consulted — and `get_reference()` then `parse_object()`s it and
            // `die(_("bad object %s"), name)`s. Only a name that resolves to
            // nothing at all reaches `setup_revisions()`'s `ambiguous argument`
            // block, which is why `git reflog show <absent-40-hex>` is `bad object`
            // in stock 2.55.0 and `git reflog show nosuchref` is not.
            //
            // Quiet, because this function already reached
            // `warn_ambiguous_refname` for `spec` above and git resolves the
            // operand once.
            if crate::objname::resolves_but_absent(repo, base) {
                eprintln!("fatal: bad object {base}");
                return Ok(Resolved::Fatal(128));
            }
            if crate::objname::resolve_quiet(repo, base).is_none() {
                return Ok(Resolved::Fatal(fatal_ambiguous(spec)));
            }
        }
    }

    // `add_reflog_for_walk()`: the operand with its selector cut off, and an empty
    // one means HEAD's own target.
    //
    // ```c
    // if (*branch == '\0') {
    //         branch = refs_resolve_refdup(get_main_ref_store(the_repository), "HEAD", 0, NULL, NULL);
    //         if (!branch)
    //                 die("no current branch");
    // }
    // ```
    // (`reflog-walk.c:187-194`)
    let branch = if base.is_empty() {
        match crate::refname::resolve_ref_reading(repo, "HEAD") {
            Some(name) => name,
            None => {
                eprintln!("fatal: no current branch");
                return Ok(Resolved::Fatal(128));
            }
        }
    } else {
        base.to_owned()
    };

    // ```c
    // reflogs = read_complete_reflog(branch);
    // if (!reflogs || reflogs->nr == 0) {
    //         int ret = repo_dwim_log(the_repository, branch, strlen(branch), NULL, &b);
    //         if (ret == 1) { branch = b; reflogs = read_complete_reflog(branch); }
    // }
    // if (!reflogs || reflogs->nr == 0)
    //         return -1;
    // ```
    // (`reflog-walk.c:195-213`). The `return -1` is ignored by `add_pending_object`,
    // so the commit is simply never queued and the walk prints nothing.
    // `add_reflog_for_walk()` prints the operand as typed, except for a bare
    // `@{…}`, where `branch` was replaced by HEAD's target before the log was read.
    let display = if base.is_empty() { branch.clone() } else { base.to_owned() };

    let (mut named, mut entries) = read_complete_reflog(repo, &branch)?;
    if entries.is_empty() {
        if let Some(dwimmed) = dwim_log(repo, &branch) {
            let (n, e) = read_complete_reflog(repo, &dwimmed)?;
            named = n;
            entries = e;
        }
    }
    if entries.is_empty() {
        return Ok(Resolved::Empty);
    }

    match selector {
        None => Ok(Resolved::Section(Section {
            display,
            full: named,
            start: 0,
            selector: SelectorKind::None,
            entries,
        })),
        Some(Selector::Index(n)) => {
            if n >= entries.len() {
                eprintln!("fatal: log for '{base}' only has {} entries", entries.len());
                return Ok(Resolved::Fatal(128));
            }
            Ok(Resolved::Section(Section {
                display,
                full: named,
                start: n,
                selector: SelectorKind::Index,
                entries,
            }))
        }
        Some(Selector::Date(text)) => {
            // `object-name.c:780`: approxidate reads the selector, and only a value with nothing
            // date-like in it is ambiguous. Dots need no rewriting — approxidate tokenizes on
            // every non-alphanumeric byte, so `2.days.ago` is native to it.
            let (target, error) = crate::date::approxidate_careful(text);
            if error {
                return Ok(Resolved::Fatal(fatal_ambiguous(spec)));
            }

            // Entries are newest-first; the answer is the newest one that was
            // already current at `target`.
            let start = entries
                .iter()
                .position(|e| e.time.seconds <= target)
                .unwrap_or(entries.len());
            if start == entries.len() {
                if let Some(oldest) = entries.last() {
                    // `show_date(co_time, co_tz, DATE_MODE(RFC2822))`
                    // (`object-name.c:797-799`), which writes the day of the month
                    // unpadded — `7 Apr`, not `07 Apr`.
                    eprintln!(
                        "warning: log for '{base}' only goes back to {}",
                        super::log::show_date_rfc2822(oldest.time.seconds, oldest.time.offset)
                    );
                }
            }
            Ok(Resolved::Section(Section {
                display,
                full: named,
                start,
                // A date selector switches only this section's column to date form.
                selector: SelectorKind::Date,
                entries,
            }))
        }
    }
}

/// `read_complete_reflog()` (`reflog-walk.c:68-103`): the entries `git log -g` walks
/// for one operand, and the name it prints them under.
///
/// The name is the operand as handed in — `reflogs->ref = xstrdup(ref)` happens
/// before any of the fallbacks — so `git reflog show dup` prints `dup@{0}` even
/// though the entries came out of `logs/refs/heads/dup`. Only the *content* falls
/// back, and it does so down a fixed chain:
///
/// ```c
/// reflogs->ref = xstrdup(ref);
/// refs_for_each_reflog_ent(refs, ref, read_one_reflog, reflogs);
/// if (reflogs->nr == 0) {
///         name = refs_resolve_refdup(refs, ref, RESOLVE_REF_READING, NULL, NULL);
///         if (name)
///                 refs_for_each_reflog_ent(refs, name, read_one_reflog, reflogs);
/// }
/// if (reflogs->nr == 0) {
///         char *refname = xstrfmt("refs/%s", ref);
///         refs_for_each_reflog_ent(refs, refname, read_one_reflog, reflogs);
///         if (reflogs->nr == 0) {
///                 refname = xstrfmt("refs/heads/%s", ref);
///                 refs_for_each_reflog_ent(refs, refname, read_one_reflog, reflogs);
///         }
/// }
/// ```
///
/// Note what the chain does *not* contain: `refs/tags/` and `refs/remotes/`. A tag
/// and a branch may share a name and only the branch's log is ever found here.
fn read_complete_reflog(repo: &gix::Repository, r#ref: &str) -> Result<(String, Vec<Entry>)> {
    let mut entries = read_log_of(repo, r#ref)?;
    if entries.is_empty() {
        if let Some(resolved) = crate::refname::resolve_ref_reading(repo, r#ref) {
            entries = read_log_of(repo, &resolved)?;
        }
    }
    if entries.is_empty() {
        entries = read_log_of(repo, &format!("refs/{}", r#ref))?;
    }
    if entries.is_empty() {
        entries = read_log_of(repo, &format!("refs/heads/{}", r#ref))?;
    }
    Ok((r#ref.to_owned(), entries))
}

/// `refs_for_each_reflog_ent()` for one exact ref name, flipped into git's
/// newest-first order. A name with no log file reads as no entries, which is what
/// drives [`read_complete_reflog`]'s fallback chain.
fn read_log_of(repo: &gix::Repository, name: &str) -> Result<Vec<Entry>> {
    Ok(read_entries(repo, name)?.unwrap_or_default())
}

/// `refs_for_each_reflog_ent()` for one *exact* ref name, flipped into git's
/// newest-first order. `None` means there is no log file under that name.
///
/// The name is taken literally: the files backend maps it straight onto
/// `logs/<name>` and never dwims. That matters because [`read_complete_reflog`]
/// walks a fallback chain of its own and a lookup that quietly answered under
/// some other name would short-circuit it — `git reflog show dup` would then read
/// `refs/tags/dup`'s (absent) log and stop, where git goes on to
/// `refs/heads/dup`.
fn read_entries(repo: &gix::Repository, name: &str) -> Result<Option<Vec<Entry>>> {
    let mut buf: Vec<u8> = Vec::new();
    let Ok(Some(iter)) = repo.refs.reflog_iter(name, &mut buf) else {
        return Ok(None);
    };
    let mut entries: Vec<Entry> = Vec::new();
    for line in iter {
        let line = line.map_err(|e| anyhow!("{name}: bad reflog line: {e}"))?;
        entries.push(Entry {
            oid: line.new_oid(),
            who_name: line.signature.name.to_vec(),
            who_email: line.signature.email.to_vec(),
            time: line.signature.time().ok().unwrap_or_default(),
            message: line.message.to_vec(),
        });
    }
    entries.reverse();
    Ok(Some(entries))
}

/// `%gd`'s short ref: `refs_shorten_unambiguous_ref(refs, ref, 0)`.
///
/// ```c
/// if (!commit_reflog->reflogs->short_ref)
///         commit_reflog->reflogs->short_ref
///                 = refs_shorten_unambiguous_ref(get_main_ref_store(the_repository),
///                                                commit_reflog->reflogs->ref,
///                                                0);
/// ```
/// (`reflog-walk.c:249-255`)
///
/// The reflog walker is the one caller that passes `strict = 0`, so a candidate
/// here only has to survive the rules *before* the one that produced it. It is
/// still not a prefix strip: `refs/remotes/origin/HEAD` shortens to `origin`
/// (rule 5 carries the `/HEAD` suffix), and `refs/heads/dup` alongside
/// `refs/tags/dup` stays `heads/dup`.
pub(crate) fn shorten_ref_unambiguous(repo: &gix::Repository, full: &str) -> String {
    crate::refname::shorten_unambiguous_str(repo, full, false)
}

/// The repository-free approximation [`crate::porcelain::log`] still calls for
/// `%gd` on a `git log -g` walk — a plain category strip, which is *not* what
/// `reflog-walk.c:252` does. It cannot consult the ref store for ambiguity, so
/// `refs/remotes/origin/HEAD` comes out as `origin/HEAD` where stock prints
/// `origin`. Use [`shorten_ref_unambiguous`] wherever a repository is in hand.
pub(crate) fn shorten_ref(full: &str) -> String {
    if full == "HEAD" {
        return full.to_owned();
    }
    if full == "refs/stash" {
        return "stash".to_owned();
    }
    for prefix in ["refs/heads/", "refs/remotes/", "refs/tags/"] {
        if let Some(rest) = full.strip_prefix(prefix) {
            return rest.to_owned();
        }
    }
    full.strip_prefix("refs/").unwrap_or(full).to_owned()
}

// ---------------------------------------------------------------------------
// ref sets
// ---------------------------------------------------------------------------

/// Every full ref name in the repository, sorted, whatever its kind.
fn all_ref_names(repo: &gix::Repository) -> Result<Vec<String>> {
    let platform = repo.references()?;
    let mut names: Vec<String> = Vec::new();
    for reference in platform.all()? {
        let Ok(reference) = reference else { continue };
        names.push(reference.name().as_bstr().to_str_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

/// `--all`: every ref that owns a reflog, then `HEAD`.
fn expand_all(repo: &gix::Repository, excludes: &[String]) -> Result<Vec<Section>> {
    let mut sections = Vec::new();
    for name in all_ref_names(repo)? {
        if excluded(&name, excludes) {
            continue;
        }
        if let Some(entries) = read_entries(repo, &name)? {
            sections.push(Section {
                display: name.clone(),
                full: name,
                start: 0,
                selector: SelectorKind::None,
                entries,
            });
        }
    }
    if let Some(entries) = read_entries(repo, "HEAD")? {
        sections.push(Section {
            display: "HEAD".to_owned(),
            full: "HEAD".to_owned(),
            start: 0,
            selector: SelectorKind::None,
            entries,
        });
    }
    Ok(sections)
}

/// `--branches`/`--tags`/`--remotes`: names are printed with the prefix stripped.
fn expand_prefixed(
    repo: &gix::Repository,
    prefix: &str,
    pattern: Option<&str>,
    excludes: &[String],
) -> Result<Vec<Section>> {
    let normalized = pattern.map(normalize_glob);
    let mut sections = Vec::new();
    for name in all_ref_names(repo)? {
        let Some(short) = name.strip_prefix(prefix).map(str::to_owned) else {
            continue;
        };
        if excluded(&name, excludes) {
            continue;
        }
        if let Some(pat) = &normalized {
            if !wildmatch(pat.as_bytes(), short.as_bytes()) {
                continue;
            }
        }
        if let Some(entries) = read_entries(repo, &name)? {
            sections.push(Section {
                display: short,
                full: name,
                start: 0,
                selector: SelectorKind::None,
                entries,
            });
        }
    }
    Ok(sections)
}

/// `--glob=<pat>`: matched against the full ref name, which is also what prints.
fn expand_glob(
    repo: &gix::Repository,
    pattern: &str,
    excludes: &[String],
) -> Result<Vec<Section>> {
    let normalized = normalize_glob(pattern);
    let mut sections = Vec::new();
    for name in all_ref_names(repo)? {
        if excluded(&name, excludes) {
            continue;
        }
        if !wildmatch(normalized.as_bytes(), name.as_bytes()) {
            continue;
        }
        if let Some(entries) = read_entries(repo, &name)? {
            sections.push(Section {
                display: name.clone(),
                full: name,
                start: 0,
                selector: SelectorKind::None,
                entries,
            });
        }
    }
    Ok(sections)
}

fn ref_prefix(flag: &str) -> &'static str {
    match flag {
        "--tags" => "refs/tags/",
        "--remotes" => "refs/remotes/",
        _ => "refs/heads/",
    }
}

/// `--exclude=` patterns are matched verbatim, without the `/*` completion that
/// `--glob` applies — that is how git's `ref_excluded()` behaves.
fn excluded(name: &str, excludes: &[String]) -> bool {
    excludes
        .iter()
        .any(|pat| wildmatch(pat.as_bytes(), name.as_bytes()))
}

/// git's `normalize_glob_ref()`: a pattern with no `*` matches a whole subtree.
fn normalize_glob(pattern: &str) -> String {
    if !pattern.contains('*') {
        format!("{}/*", pattern.trim_end_matches('/'))
    } else if pattern.ends_with('/') {
        format!("{pattern}*")
    } else {
        pattern.to_owned()
    }
}

/// git's wildmatch without `WM_PATHNAME`, so `*` also matches `/`.
fn wildmatch(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    // Backtrack point for the most recent `*`.
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(b'?') => {
                p += 1;
                t += 1;
            }
            Some(b'[') => match bracket_match(pattern, p, text[t]) {
                Some(next) => {
                    p = next;
                    t += 1;
                }
                None => match star {
                    Some((sp, st)) => {
                        p = sp + 1;
                        t = st + 1;
                        star = Some((sp, st + 1));
                    }
                    None => return false,
                },
            },
            Some(&c) if c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    p = sp + 1;
                    t = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    while pattern.get(p) == Some(&b'*') {
        p += 1;
    }
    p == pattern.len()
}

/// Match one `[...]` class at `open` against `c`, returning the index just past
/// the closing `]` on success.
fn bracket_match(pattern: &[u8], open: usize, c: u8) -> Option<usize> {
    let mut i = open + 1;
    let negated = matches!(pattern.get(i), Some(b'!') | Some(b'^'));
    if negated {
        i += 1;
    }
    let mut hit = false;
    let mut first = true;
    while i < pattern.len() {
        if pattern[i] == b']' && !first {
            return (hit != negated).then_some(i + 1);
        }
        first = false;
        let lo = pattern[i];
        if pattern.get(i + 1) == Some(&b'-') && pattern.get(i + 2).is_some_and(|&h| h != b']') {
            let hi = pattern[i + 2];
            if (lo..=hi).contains(&c) {
                hit = true;
            }
            i += 3;
        } else {
            if lo == c {
                hit = true;
            }
            i += 1;
        }
    }
    // Unterminated class: git treats the `[` as a literal.
    (c == b'[').then_some(open + 1)
}

// ---------------------------------------------------------------------------
// option value parsing
// ---------------------------------------------------------------------------

enum DateMode {
    Known(DateFormat),
    Unimplemented,
    Unknown,
}

/// Classify a `--date=` value. Anything git would reject outright is `Unknown`.
fn parse_date_mode(value: &str) -> DateMode {
    if value.starts_with("format:") || value.starts_with("format-local:") {
        return DateMode::Unimplemented;
    }
    let (base, local) = match value.strip_suffix("-local") {
        Some(base) => (base, true),
        None => (value, false),
    };
    // Bare `local` is git's shorthand for the default layout in the local zone.
    let (base, local) = if base == "local" {
        ("default", true)
    } else {
        (base, local)
    };
    let mut iso_strict = false;
    let fmt: TimeFormat = match base {
        // The local rendering of the default layout drops the zone offset.
        "" | "default" if local => DEFAULT_LOCAL.into(),
        "" | "default" => tfmt::DEFAULT.into(),
        "raw" => tfmt::RAW,
        "unix" => tfmt::UNIX,
        "short" => tfmt::SHORT.into(),
        "iso" | "iso8601" => tfmt::ISO8601.into(),
        "iso-strict" | "iso8601-strict" => {
            iso_strict = true;
            tfmt::ISO8601_STRICT.into()
        }
        "rfc" | "rfc2822" => tfmt::RFC2822.into(),
        // Recognized by git, but these need the current time, which is not a
        // property of the entry being rendered.
        "relative" | "human" => return DateMode::Unimplemented,
        _ => return DateMode::Unknown,
    };
    DateMode::Known(DateFormat {
        fmt,
        local,
        iso_strict,
    })
}

enum Pretty {
    Oneline,
    Builtin(Builtin),
    Custom(String),
    Unimplemented,
    Invalid,
    /// The `die()` message for a `pretty.<name>` alias chain that loops.
    Cycle(String),
}

/// Classify a `--pretty=`/`--format=` value the way git's `get_commit_format()`
/// does (pretty.c:190-222).
///
/// The three shortcuts — a `format:` prefix, then an empty value or a `tformat:`
/// prefix or a `%` anywhere — are tried in git's order and never consult config.
/// Everything else goes to the shared format table, so a name resolves as a
/// case-insensitive shortest prefix (`--pretty=one` is `oneline`) and a
/// `pretty.<name>` key is picked up along with the built-ins.
fn classify_pretty(repo: &gix::Repository, value: &str) -> Pretty {
    use super::pretty_formats::{Builtin as B, Resolved};

    if let Some(rest) = value.strip_prefix("format:") {
        return Pretty::Custom(rest.to_owned());
    }
    if value.is_empty() {
        return Pretty::Custom(String::new());
    }
    if let Some(rest) = value.strip_prefix("tformat:") {
        return Pretty::Custom(rest.to_owned());
    }
    if value.contains('%') {
        return Pretty::Custom(value.to_owned());
    }
    match super::pretty_formats::resolve(Some(repo), value) {
        Err(cycle) => Pretty::Cycle(cycle.message()),
        Ok(None) => Pretty::Invalid,
        Ok(Some(Resolved::Builtin(b))) => match b {
            B::Oneline => Pretty::Oneline,
            B::Medium => Pretty::Builtin(Builtin::Medium),
            B::Short => Pretty::Builtin(Builtin::Short),
            B::Full => Pretty::Builtin(Builtin::Full),
            B::Fuller => Pretty::Builtin(Builtin::Fuller),
            B::Raw => Pretty::Builtin(Builtin::Raw),
            B::Reference => Pretty::Builtin(Builtin::Reference),
            // The mbox/patch formats need git's whole email driver; still deferred.
            B::Email | B::MboxRd => Pretty::Unimplemented,
        },
        Ok(Some(Resolved::User { format, .. })) => Pretty::Custom(format),
    }
}

/// Whether a user `--pretty`/`--format` string uses the `%d`/`%D` decoration,
/// walking `%`-escapes so a two-char placeholder (`%gd`, `%ad`), a literal `%%d`,
/// or a `%xNN` hex byte is not mistaken for a bare `%d`.
fn format_uses_decoration(fmt: &str) -> bool {
    let b = fmt.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            i += 1;
            continue;
        }
        match b.get(i + 1) {
            Some(b'd') | Some(b'D') => return true,
            Some(b'x') => i += 4, // `%xNN` hex escape — skip both hex digits
            Some(_) => i += 2,
            None => i += 1,
        }
    }
    false
}

/// The first placeholder in `fmt` that this renderer does not implement.
fn unsupported_placeholder(fmt: &str) -> Option<String> {
    let b = fmt.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            i += 1;
            continue;
        }
        let Some(&next) = b.get(i + 1) else {
            return Some("%".to_owned());
        };
        match next {
            b'n' | b'%' => i += 2,
            // `%d`/`%D` are the ref decorations (parenthesised / bare).
            b'H' | b'T' | b'P' | b'p' | b'h' | b's' | b'd' | b'D' => i += 2,
            // The column atoms `%<(<N>)`, `%>(<N>)`, `%><(<N>)`, `%>>(<N>)` and
            // the `%w(…)` wrap atom are validated where they are expanded: a
            // malformed one is not a placeholder at all, and git prints it
            // literally rather than failing. Skipping just the two bytes leaves
            // the parenthesised tail to be scanned as literal text, which carries
            // no `%` and so cannot be mistaken for another placeholder.
            b'<' | b'>' | b'w' => i += 2,
            b'x' => {
                if b.get(i + 2).is_some_and(u8::is_ascii_hexdigit)
                    && b.get(i + 3).is_some_and(u8::is_ascii_hexdigit)
                {
                    i += 4;
                } else {
                    return Some("%x".to_owned());
                }
            }
            b'a' | b'c' | b'g' => {
                let Some(&third) = b.get(i + 2) else {
                    return Some(format!("%{}", next as char));
                };
                let ok = match next {
                    b'g' => matches!(third, b'd' | b'D' | b'n' | b'e' | b's'),
                    // `%ad`/`%cd` plus the fixed-format date atoms `%ai`/`%aI`
                    // (ISO), `%at` (unix), and `%ar`/`%cr` (relative).
                    _ => matches!(third, b'n' | b'e' | b'd' | b'i' | b'I' | b't' | b'r'),
                };
                if ok {
                    i += 3;
                } else {
                    return Some(format!("%{}{}", next as char, third as char));
                }
            }
            other => return Some(format!("%{}", other as char)),
        }
    }
    None
}

/// Whether the selected output renders a field date — the `%ad`/`%cd` placeholders
/// or the `Date:`/`AuthorDate:`/`CommitDate:` header lines. That is the only place
/// `log.date` takes effect: the reflog selector, the `reference` short-date and the
/// `raw` verbatim times are all independent of it (verified against git 2.55.0).
fn renders_field_date(out: &OutFmt) -> bool {
    match out {
        OutFmt::Oneline => false,
        OutFmt::Builtin(b) => matches!(b, Builtin::Medium | Builtin::Fuller),
        OutFmt::Custom(fmt) => custom_has_field_date(fmt),
    }
}

/// Whether a validated `--format`/`--pretty` string contains a `%ad` or `%cd`
/// placeholder. Walks placeholders the way [`expand_format`] does so a literal
/// `%%ad` or a `%an`/`%cn` cannot be mistaken for a field date.
fn custom_has_field_date(fmt: &str) -> bool {
    let b = fmt.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            i += 1;
            continue;
        }
        match b.get(i + 1) {
            // `%x<hh>` is a four-byte hex escape.
            Some(b'x') => i += 4,
            // `%g<x>` and `%a<x>`/`%c<x>` are three bytes; only `%ad`/`%cd` are dates.
            Some(b'g') => i += 3,
            Some(b'a') | Some(b'c') => {
                if b.get(i + 2) == Some(&b'd') {
                    return true;
                }
                i += 3;
            }
            // Everything else recognized here (`%H %h %T %P %p %s %n %%`) is two bytes.
            _ => i += 2,
        }
    }
    false
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

/// Expand a validated `--format` string for one entry.
///
/// git's `repo_format_commit_message()` driver loop (pretty.c:2014), which
/// `builtin/reflog.c` reaches through `show_reflog()`'s `pretty_print_commit()`:
/// literal text is copied, `%%` is expanded by `strbuf_expand_step()` before
/// `format_commit_item()` is reached (so it is neither padded nor spends a
/// pending field), and every other `%` placeholder goes through
/// [`expand_one_placeholder`] — directly, or into a measured buffer when a
/// `%<`/`%>` atom left a field pending.
///
/// `format_and_pad_commit()`'s `%C…` chain is absent because
/// [`unsupported_placeholder`] refuses `%C` up front, so a colour atom can never
/// open a chain here.
#[allow(clippy::too_many_arguments)]
fn expand_format(
    repo: &gix::Repository,
    fmt: &str,
    section: &Section,
    entry: &Entry,
    selector: &str,
    opts: &Opts,
    field_fmt: DateFormat,
    fallback_len: usize,
    decorations: Option<&Decorations>,
) -> Vec<u8> {
    let commit = repo.find_commit(entry.oid).ok();
    let b = fmt.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(fmt.len() + 32);
    // The deferred state `struct format_commit_context` carries: a column field a
    // `%<`/`%>` atom is holding open, and the `%w()` wrap parameters.
    let mut pad = PadState::default();
    let mut wrap = WrapState::default();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        if b.get(i + 1) == Some(&b'%') {
            out.push(b'%');
            i += 2;
            continue;
        }
        // The cursor sits on the placeholder character; the expander advances it
        // past whatever it consumes and leaves it alone when it consumes nothing,
        // which is how git rescans from that character.
        let mut at = i + 1;
        let args = PlaceholderArgs {
            repo,
            section,
            entry,
            selector,
            opts,
            field_fmt,
            fallback_len,
            decorations,
            commit: &commit,
        };
        if pad.flush == FlushType::None {
            if !expand_one_placeholder(&mut out, b, &mut at, &args, &mut pad, &mut wrap) {
                out.push(b'%');
            }
            i = at;
            continue;
        }
        // `format_and_pad_commit()`: the placeholder renders into a buffer of its
        // own so its *display* width can be measured. `padding` is read before it
        // expands, so a nested `%<(…)` retargets the next field, not this one.
        let padding = pad.padding;
        let mut local: Vec<u8> = Vec::new();
        let consumed = expand_one_placeholder(&mut local, b, &mut at, &args, &mut pad, &mut wrap);
        pad.apply(&mut out, local, padding, 0);
        if !consumed {
            out.push(b'%');
        }
        i = at;
    }
    // `repo_format_commit_message()` closes with a rewrap to width 0, which wraps
    // whatever a trailing `%w()` was still governing.
    wrap.rewrap_message_tail(&mut out, 0, 0, 0);
    out
}

/// Everything one reflog entry's placeholders can read, bundled so the expander
/// keeps a manageable signature.
struct PlaceholderArgs<'a> {
    repo: &'a gix::Repository,
    section: &'a Section,
    entry: &'a Entry,
    selector: &'a str,
    opts: &'a Opts,
    field_fmt: DateFormat,
    fallback_len: usize,
    decorations: Option<&'a Decorations>,
    /// The entry's commit, when it still resolves — `%s`, `%T` and the person
    /// placeholders expand to nothing when it does not.
    commit: &'a Option<gix::Commit<'a>>,
}

/// `format_commit_one()`: expand the single placeholder at `b[*at]`, advancing
/// `*at` past whatever it consumes.
///
/// `false` is git's "consumed 0 bytes" — nothing was written and `*at` is left on
/// the placeholder character, so the caller prints the `%` and rescans.
fn expand_one_placeholder(
    out: &mut Vec<u8>,
    b: &[u8],
    at: &mut usize,
    args: &PlaceholderArgs<'_>,
    pad: &mut PadState,
    wrap: &mut WrapState,
) -> bool {
    let PlaceholderArgs {
        repo,
        section,
        entry,
        selector,
        opts,
        field_fmt,
        fallback_len,
        decorations,
        commit,
    } = *args;
    let (field_fmt, fallback_len) = (field_fmt, fallback_len);

    // The column atoms `%<(<N>)`, `%>(<N>)`, `%><(<N>)`, `%>>(<N>)` and their
    // `|`/`trunc`/`ltrunc`/`mtrunc` forms expand to nothing and leave the field
    // pending; `%w(…)` re-wraps everything emitted after it. Both are validated
    // here rather than by `unsupported_placeholder`, because a malformed one is
    // not a placeholder at all and git prints it literally.
    match b.get(*at) {
        Some(b'<' | b'>') => {
            return match pad.parse(b, *at) {
                Some(consumed) => {
                    *at += consumed;
                    true
                }
                None => false,
            };
        }
        Some(b'w') => {
            return match wrap.parse_and_apply(out, b, *at) {
                Some(consumed) => {
                    *at += consumed;
                    true
                }
                None => false,
            };
        }
        _ => {}
    }

    // `i` tracks git's cursor on the `%` itself, so the per-atom widths below read
    // as they do in `pretty.c`. Indexing stays on bytes so a multi-byte literal in
    // the format string can never split a `char` boundary.
    let mut i = *at - 1;
    {
        let one = b.get(i + 1).copied();
        let two = b.get(i + 2).copied();
        match (one, two) {
            (Some(b'g'), Some(kind @ (b'd' | b'D'))) => {
                // `%gd` is the short selector (`refs_shorten_unambiguous_ref`
                // against the live ref set, non-strict), `%gD` the full ref. Both
                // are independent of the oneline path, which prints the ref as it
                // was typed.
                let name = if kind == b'd' {
                    shorten_ref_unambiguous(repo, &section.full)
                } else {
                    section.full.clone()
                };
                out.extend_from_slice(name.as_bytes());
                out.extend_from_slice(format!("@{{{selector}}}").as_bytes());
                i += 3;
            }
            (Some(b'g'), Some(b'n')) => {
                out.extend_from_slice(&entry.who_name);
                i += 3;
            }
            (Some(b'g'), Some(b'e')) => {
                out.extend_from_slice(&entry.who_email);
                i += 3;
            }
            (Some(b'g'), Some(b's')) => {
                out.extend_from_slice(&entry.message);
                i += 3;
            }
            (Some(who @ (b'a' | b'c')), Some(field @ (b'n' | b'e' | b'd' | b'i' | b'I' | b't' | b'r'))) => {
                // Bound the signature to `commit` explicitly: it borrows the
                // commit's decoded buffer, so it cannot escape a closure.
                let sig = match &commit {
                    Some(c) if who == b'a' => c.author().ok(),
                    Some(c) => c.committer().ok(),
                    None => None,
                };
                if let Some(sig) = sig {
                    match field {
                        b'n' => out.extend_from_slice(sig.name),
                        b'e' => out.extend_from_slice(sig.email),
                        b'r' => {
                            let t = sig.time().ok().unwrap_or_default();
                            let rel = crate::date::show_date_relative(t.seconds, crate::date::now_seconds());
                            out.extend_from_slice(rel.as_bytes());
                        }
                        b't' => {
                            let t = sig.time().ok().unwrap_or_default();
                            out.extend_from_slice(t.seconds.to_string().as_bytes());
                        }
                        b'i' | b'I' => {
                            let t = sig.time().ok().unwrap_or_default();
                            let df = if field == b'i' {
                                DateFormat { fmt: tfmt::ISO8601.into(), local: false, iso_strict: false }
                            } else {
                                DateFormat { fmt: tfmt::ISO8601_STRICT.into(), local: false, iso_strict: true }
                            };
                            out.extend_from_slice(df.render(t).as_bytes());
                        }
                        // `d`: the `--date=`/`log.date` format.
                        _ => {
                            let t = sig.time().ok().unwrap_or_default();
                            out.extend_from_slice(field_fmt.render(t).as_bytes());
                        }
                    }
                }
                i += 3;
            }
            (Some(b'n'), _) => {
                out.push(b'\n');
                i += 2;
            }
            // `%D` is the bare ref decoration; `%d` wraps it in ` (...)`. Both are
            // empty when nothing points at the entry's commit.
            (Some(b'D'), _) => {
                if let Some(text) = decorations.and_then(|d| d.bare_for_commit(entry.oid)) {
                    out.extend_from_slice(text.as_bytes());
                }
                i += 2;
            }
            (Some(b'd'), _) => {
                if let Some(text) = decorations.and_then(|d| d.bare_for_commit(entry.oid)) {
                    out.extend_from_slice(b" (");
                    out.extend_from_slice(text.as_bytes());
                    out.push(b')');
                }
                i += 2;
            }
            (Some(b'H'), _) => {
                out.extend_from_slice(entry.oid.to_string().as_bytes());
                i += 2;
            }
            (Some(b'h'), _) => {
                out.extend_from_slice(
                    abbrev_id(repo, entry.oid, &opts.abbrev, fallback_len).as_bytes(),
                );
                i += 2;
            }
            (Some(b'T'), _) => {
                let tree = match &commit {
                    Some(c) => c.tree_id().ok(),
                    None => None,
                };
                if let Some(tree) = tree {
                    out.extend_from_slice(tree.detach().to_string().as_bytes());
                }
                i += 2;
            }
            (Some(kind @ (b'P' | b'p')), _) => {
                let abbreviate = kind == b'p';
                for (k, parent) in parents_of(repo, entry.oid).into_iter().enumerate() {
                    if k > 0 {
                        out.push(b' ');
                    }
                    if abbreviate {
                        out.extend_from_slice(
                            abbrev_id(repo, parent, &opts.abbrev, fallback_len).as_bytes(),
                        );
                    } else {
                        out.extend_from_slice(parent.to_string().as_bytes());
                    }
                }
                i += 2;
            }
            (Some(b's'), _) => {
                let summary = match &commit {
                    Some(c) => c.message().ok().map(|m| m.summary().to_vec()),
                    None => None,
                };
                if let Some(summary) = summary {
                    out.extend_from_slice(&summary);
                }
                i += 2;
            }
            (Some(b'x'), _) if i + 4 <= b.len() => {
                if let Ok(hex) = std::str::from_utf8(&b[i + 2..i + 4]) {
                    if let Ok(byte) = u8::from_str_radix(hex, 16) {
                        out.push(byte);
                    }
                }
                i += 4;
            }
            // `format_commit_one()` consumed nothing — a trailing `%`, or a
            // sequence `unsupported_placeholder` let through that this expander
            // does not write. The caller prints the `%` and rescans.
            _ => return false,
        }
    }
    *at = i;
    true
}

/// The parent ids of the commit an entry points at, empty when it is not a
/// readable commit.
fn parents_of(repo: &gix::Repository, id: ObjectId) -> Vec<ObjectId> {
    match repo.find_commit(id) {
        Ok(commit) => commit.parent_ids().map(|p| p.detach()).collect(),
        Err(_) => Vec::new(),
    }
}

fn is_merge(repo: &gix::Repository, id: ObjectId) -> bool {
    parents_of(repo, id).len() >= 2
}

/// Render one entry in a built-in multi-line `--pretty` format, including the
/// `Reflog:`/`Reflog message:` decorations git adds under `--walk-reflogs`. The
/// returned block is already newline-terminated; the caller handles the blank
/// line between entries and any following diff.
#[allow(clippy::too_many_arguments)]
fn build_builtin_block(
    repo: &gix::Repository,
    kind: Builtin,
    section: &Section,
    entry: &Entry,
    selector: &str,
    opts: &Opts,
    field_fmt: DateFormat,
    fallback_len: usize,
    decorations: Option<&Decorations>,
) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let commit = repo.find_commit(entry.oid).ok();
    let subject = || {
        commit
            .as_ref()
            .and_then(|c| c.message().ok().map(|m| m.summary().to_vec()))
            .unwrap_or_default()
    };

    // `reference` is a one-line format with no reflog header.
    if let Builtin::Reference = kind {
        let id = abbrev_id(repo, entry.oid, &opts.abbrev, fallback_len);
        let date = commit
            .as_ref()
            .and_then(|c| c.author().ok())
            .map(|a| DateFormat::plain(tfmt::SHORT).render(a.time().ok().unwrap_or_default()))
            .unwrap_or_default();
        out.extend_from_slice(id.as_bytes());
        out.extend_from_slice(b" (");
        out.extend_from_slice(&subject());
        out.extend_from_slice(b", ");
        out.extend_from_slice(date.as_bytes());
        out.extend_from_slice(b")\n");
        return out;
    }

    // `commit <id>`: `raw` prints the full hash, the rest honour `--abbrev-commit`.
    out.extend_from_slice(b"commit ");
    let id = match kind {
        Builtin::Raw => entry.oid.to_string(),
        _ => abbrev_id(repo, entry.oid, &opts.abbrev, fallback_len),
    };
    out.extend_from_slice(id.as_bytes());
    if let Some(deco) = decorations {
        if let Some(text) = deco.for_commit(entry.oid) {
            out.push(b' ');
            out.extend_from_slice(text.as_bytes());
        }
    }
    out.push(b'\n');

    // The reflog header lines, common to every multi-line format.
    out.extend_from_slice(b"Reflog: ");
    out.extend_from_slice(section.display.as_bytes());
    out.extend_from_slice(format!("@{{{selector}}} (").as_bytes());
    out.extend_from_slice(&entry.who_name);
    out.extend_from_slice(b" <");
    out.extend_from_slice(&entry.who_email);
    out.extend_from_slice(b">)\n");
    out.extend_from_slice(b"Reflog message: ");
    out.extend_from_slice(&entry.message);
    out.push(b'\n');

    let parents = parents_of(repo, entry.oid);
    match kind {
        Builtin::Raw => {
            if let Some(c) = &commit {
                if let Ok(tree) = c.tree_id() {
                    out.extend_from_slice(format!("tree {}\n", tree.detach()).as_bytes());
                }
            }
            for parent in &parents {
                out.extend_from_slice(format!("parent {parent}\n").as_bytes());
            }
            if let Some(c) = &commit {
                if let Ok(a) = c.author() {
                    append_raw_ident(&mut out, b"author ", &a);
                }
                if let Ok(cm) = c.committer() {
                    append_raw_ident(&mut out, b"committer ", &cm);
                }
            }
        }
        _ => {
            // `Merge: <abbrev parents>` for a merge commit.
            if parents.len() > 1 {
                out.extend_from_slice(b"Merge:");
                for parent in &parents {
                    out.push(b' ');
                    out.extend_from_slice(
                        abbrev_id(repo, *parent, &opts.abbrev, fallback_len).as_bytes(),
                    );
                }
                out.push(b'\n');
            }
            let author = commit.as_ref().and_then(|c| c.author().ok());
            let committer = commit.as_ref().and_then(|c| c.committer().ok());
            match kind {
                Builtin::Medium => {
                    append_ident(&mut out, b"Author: ", author.as_ref());
                    append_date(&mut out, b"Date:   ", author.as_ref(), field_fmt);
                }
                Builtin::Short => append_ident(&mut out, b"Author: ", author.as_ref()),
                Builtin::Full => {
                    append_ident(&mut out, b"Author: ", author.as_ref());
                    append_ident(&mut out, b"Commit: ", committer.as_ref());
                }
                Builtin::Fuller => {
                    append_ident(&mut out, b"Author:     ", author.as_ref());
                    append_date(&mut out, b"AuthorDate: ", author.as_ref(), field_fmt);
                    append_ident(&mut out, b"Commit:     ", committer.as_ref());
                    append_date(&mut out, b"CommitDate: ", committer.as_ref(), field_fmt);
                }
                Builtin::Raw | Builtin::Reference => unreachable!("handled above"),
            }
        }
    }

    // A blank line, then the message body — the folded subject only for `short`,
    // the whole raw message otherwise, indented four spaces per line.
    out.push(b'\n');
    if let Builtin::Short = kind {
        let mut body = subject();
        body.push(b'\n');
        indent_body(&mut out, &body);
    } else {
        let body = commit
            .as_ref()
            .and_then(|c| c.message_raw().ok().map(|m| m.to_vec()))
            .unwrap_or_default();
        indent_body(&mut out, &body);
    }
    out
}

/// git's raw `author`/`committer` line: `<label><name> <email> <raw-time>`, where
/// the time is copied verbatim from the object (`<seconds> <tz>`).
fn append_raw_ident(out: &mut Vec<u8>, label: &[u8], sig: &gix::actor::SignatureRef<'_>) {
    out.extend_from_slice(label);
    out.extend_from_slice(sig.name);
    out.extend_from_slice(b" <");
    out.extend_from_slice(sig.email);
    out.extend_from_slice(b"> ");
    out.extend_from_slice(sig.time.as_bytes());
    out.push(b'\n');
}

/// git's `Author: <name> <email>` identity line.
fn append_ident(out: &mut Vec<u8>, label: &[u8], sig: Option<&gix::actor::SignatureRef<'_>>) {
    out.extend_from_slice(label);
    if let Some(sig) = sig {
        out.extend_from_slice(sig.name);
        out.extend_from_slice(b" <");
        out.extend_from_slice(sig.email);
        out.push(b'>');
    }
    out.push(b'\n');
}

/// git's `Date:   <formatted>` line, in the selector's `--date` layout.
fn append_date(
    out: &mut Vec<u8>,
    label: &[u8],
    sig: Option<&gix::actor::SignatureRef<'_>>,
    fmt: DateFormat,
) {
    out.extend_from_slice(label);
    if let Some(sig) = sig {
        let time = sig.time().ok().unwrap_or_default();
        out.extend_from_slice(fmt.render(time).as_bytes());
    }
    out.push(b'\n');
}

/// git's `strbuf_add_lines`: prefix every line (blank ones included) of `msg`
/// with four spaces, stopping at the message end without a trailing blank line.
fn indent_body(out: &mut Vec<u8>, msg: &[u8]) {
    let mut rest = msg;
    while !rest.is_empty() {
        let (line, next) = match rest.iter().position(|&b| b == b'\n') {
            Some(p) => (&rest[..p], &rest[p + 1..]),
            None => (rest, &rest[rest.len()..]),
        };
        out.extend_from_slice(b"    ");
        out.extend_from_slice(line);
        out.push(b'\n');
        rest = next;
    }
}

// ---------------------------------------------------------------------------
// diff output
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChangeKind {
    Added,
    Deleted,
    Modified,
    Renamed,
    Copied,
}

impl ChangeKind {
    fn letter(self) -> u8 {
        match self {
            ChangeKind::Added => b'A',
            ChangeKind::Deleted => b'D',
            ChangeKind::Modified => b'M',
            ChangeKind::Renamed => b'R',
            ChangeKind::Copied => b'C',
        }
    }
}

/// One entry of git's diff queue, reduced to what the implemented formats print.
struct FileChange {
    /// The destination path, which is also the sort key git orders the queue by.
    path: Vec<u8>,
    /// The source path of a rename or copy.
    source: Option<Vec<u8>>,
    kind: ChangeKind,
    old_mode: Option<u16>,
    new_mode: Option<u16>,
    /// The pre-image blob id, `None` on the added side (git's raw format prints a
    /// null id there).
    old_oid: Option<ObjectId>,
    /// The post-image blob id, `None` on the deleted side.
    new_oid: Option<ObjectId>,
    /// `(insertions, deletions)`, or `None` when either side is binary.
    counts: Option<(u32, u32)>,
    /// Rename/copy similarity in percent.
    score: u32,
}

/// The diff of `oid` against its first parent, as git's diff queue would hold it.
///
/// Empty for a merge (`git log` does not diff merges unless asked with `-m`/`-c`,
/// and `git reflog` never asks) and for an object that is not a readable commit.
///
/// `first_parent_merges` is git's `diff_merges_default_to_first_parent()`, which
/// only `git log -g --first-parent` reaches — that is how `git stash list
/// --name-only` gets a diff at all, every stash entry being a merge commit.
fn collect_changes(
    repo: &gix::Repository,
    oid: ObjectId,
    cache: &mut gix::diff::blob::Platform,
    first_parent_merges: bool,
) -> Vec<FileChange> {
    let Ok(commit) = repo.find_commit(oid) else {
        return Vec::new();
    };
    let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
    // A merge has no single diff to show unless the caller was `git log`, whose
    // tweak hook promotes `--first-parent` to a first-parent merge diff.
    if parents.len() > 1 && !first_parent_merges {
        return Vec::new();
    }
    let Ok(new_tree) = commit.tree() else {
        return Vec::new();
    };
    let old_tree = match parents.first() {
        Some(parent) => {
            let Ok(parent) = repo.find_commit(*parent) else {
                return Vec::new();
            };
            match parent.tree() {
                Ok(tree) => tree,
                Err(_) => return Vec::new(),
            }
        }
        // A root commit is diffed against the empty tree.
        None => repo.empty_tree(),
    };

    let Ok(mut platform) = old_tree.changes() else {
        return Vec::new();
    };
    let mut changes: Vec<FileChange> = Vec::new();
    let walked = platform.for_each_to_obtain_tree(&new_tree, |change| {
        if let Some(file) = to_file_change(change, cache) {
            changes.push(file);
        }
        cache.clear_resource_cache_keep_allocation();
        Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(()))
    });
    if walked.is_err() {
        return Vec::new();
    }
    // git walks both trees in tree order, which orders full paths by raw bytes,
    // and rename detection leaves the pair in its destination's slot.
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    changes
}

/// Reduce one gitoxide change to a queue entry, dropping the tree entries that
/// gitoxide reports alongside their contents but git's recursive diff never shows.
fn to_file_change(
    change: gix::object::tree::diff::Change<'_, '_, '_>,
    cache: &mut gix::diff::blob::Platform,
) -> Option<FileChange> {
    use gix::object::tree::diff::Change as TreeChange;

    match change {
        TreeChange::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            if entry_mode.is_tree() {
                return None;
            }
            Some(FileChange {
                path: location.to_vec(),
                source: None,
                kind: ChangeKind::Added,
                old_mode: None,
                new_mode: Some(entry_mode.value()),
                old_oid: None,
                new_oid: Some(id.detach()),
                counts: if entry_mode.is_commit() {
                    Some((1, 0))
                } else {
                    blob_counts(&change, cache)
                },
                score: 0,
            })
        }
        TreeChange::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            if entry_mode.is_tree() {
                return None;
            }
            Some(FileChange {
                path: location.to_vec(),
                source: None,
                kind: ChangeKind::Deleted,
                old_mode: Some(entry_mode.value()),
                new_mode: None,
                old_oid: Some(id.detach()),
                new_oid: None,
                counts: if entry_mode.is_commit() {
                    Some((0, 1))
                } else {
                    blob_counts(&change, cache)
                },
                score: 0,
            })
        }
        TreeChange::Modification {
            location,
            previous_entry_mode,
            entry_mode,
            previous_id,
            id,
            ..
        } => {
            if entry_mode.is_tree() || previous_entry_mode.is_tree() {
                return None;
            }
            Some(FileChange {
                path: location.to_vec(),
                source: None,
                kind: ChangeKind::Modified,
                old_mode: Some(previous_entry_mode.value()),
                new_mode: Some(entry_mode.value()),
                old_oid: Some(previous_id.detach()),
                new_oid: Some(id.detach()),
                counts: if entry_mode.is_commit() || previous_entry_mode.is_commit() {
                    // A gitlink diffs as the single line `Subproject commit <id>`.
                    Some((1, 1))
                } else {
                    blob_counts(&change, cache)
                },
                score: 0,
            })
        }
        TreeChange::Rewrite {
            source_location,
            source_entry_mode,
            source_id,
            entry_mode,
            location,
            id,
            diff,
            copy,
            ..
        } => {
            if entry_mode.is_tree() || source_entry_mode.is_tree() {
                return None;
            }
            let identical = source_id.detach() == id.detach();
            Some(FileChange {
                path: location.to_vec(),
                source: Some(source_location.to_vec()),
                kind: if copy {
                    ChangeKind::Copied
                } else {
                    ChangeKind::Renamed
                },
                old_mode: Some(source_entry_mode.value()),
                new_mode: Some(entry_mode.value()),
                old_oid: Some(source_id.detach()),
                new_oid: Some(id.detach()),
                counts: if entry_mode.is_commit() || source_entry_mode.is_commit() {
                    Some(if identical { (0, 0) } else { (1, 1) })
                } else {
                    blob_counts(&change, cache)
                },
                // `diff` is absent exactly when both sides are the same object.
                score: diff.map_or(100, |d| (d.similarity * 100.0) as u32),
            })
        }
    }
}

/// Line counts for a blob-backed change; `None` when either side is binary, which
/// is what git renders as `-` in `--numstat`.
fn blob_counts(
    change: &gix::object::tree::diff::Change<'_, '_, '_>,
    cache: &mut gix::diff::blob::Platform,
) -> Option<(u32, u32)> {
    let mut platform = change.diff(cache).ok()?;
    let stats = platform.line_counts().ok().flatten()?;
    Some((stats.insertions, stats.removals))
}

/// Write the diff of one reflog entry in every selected format, in git's order:
/// the raw format first, then the name formats, then numstat, shortstat, summary.
fn append_diff(
    out: &mut Vec<u8>,
    repo: &gix::Repository,
    changes: &[FileChange],
    fmts: DiffFormats,
    abbrev: &Abbrev,
    fallback_len: usize,
) {
    if changes.is_empty() {
        return;
    }

    // `--raw`: `:<old-mode> <new-mode> <old-sha> <new-sha> <status>\t<path>`, with
    // a missing side rendered as a zero mode and an abbreviated null object id.
    if fmts.raw {
        let null = ObjectId::null(repo.object_hash());
        for change in changes {
            out.extend_from_slice(
                format!(
                    ":{:06o} {:06o} ",
                    change.old_mode.unwrap_or(0),
                    change.new_mode.unwrap_or(0)
                )
                .as_bytes(),
            );
            out.extend_from_slice(
                abbrev_id(repo, change.old_oid.unwrap_or(null), abbrev, fallback_len).as_bytes(),
            );
            out.push(b' ');
            out.extend_from_slice(
                abbrev_id(repo, change.new_oid.unwrap_or(null), abbrev, fallback_len).as_bytes(),
            );
            out.push(b' ');
            out.push(change.kind.letter());
            match &change.source {
                Some(source) => {
                    out.extend_from_slice(format!("{:03}\t", change.score).as_bytes());
                    out.extend_from_slice(&crate::quote::quoted_name_bytes(source));
                    out.push(b'\t');
                    out.extend_from_slice(&crate::quote::quoted_name_bytes(&change.path));
                }
                None => {
                    out.push(b'\t');
                    out.extend_from_slice(&crate::quote::quoted_name_bytes(&change.path));
                }
            }
            out.push(b'\n');
        }
    }

    if fmts.name_only {
        for change in changes {
            out.extend_from_slice(&crate::quote::quoted_name_bytes(&change.path));
            out.push(b'\n');
        }
    }

    if fmts.name_status {
        for change in changes {
            match &change.source {
                Some(source) => {
                    out.push(change.kind.letter());
                    out.extend_from_slice(format!("{:03}\t", change.score).as_bytes());
                    out.extend_from_slice(&crate::quote::quoted_name_bytes(source));
                    out.push(b'\t');
                }
                None => {
                    out.push(change.kind.letter());
                    out.push(b'\t');
                }
            }
            out.extend_from_slice(&crate::quote::quoted_name_bytes(&change.path));
            out.push(b'\n');
        }
    }

    if fmts.stat {
        // The same histogram `diff --stat` prints, fed the rows this module
        // already computed — widths, `{a => b}` compaction and the summary line
        // all come from there rather than from a second implementation.
        let rows: Vec<(gix::bstr::BString, gix::bstr::BString, u32, u32, bool)> = changes
            .iter()
            .map(|change| {
                let dest = gix::bstr::BString::from(change.path.clone());
                let src = change
                    .source
                    .clone()
                    .map_or_else(|| dest.clone(), gix::bstr::BString::from);
                let (ins, del) = change.counts.unwrap_or((0, 0));
                (src, dest, ins, del, change.counts.is_none())
            })
            .collect();
        super::diff::render_rows_stat(out, &rows, &super::diff_color::DiffColors::disabled());
    }

    if fmts.numstat {
        for change in changes {
            match change.counts {
                Some((insertions, deletions)) => {
                    out.extend_from_slice(format!("{insertions}\t{deletions}\t").as_bytes());
                }
                None => out.extend_from_slice(b"-\t-\t"),
            }
            out.extend_from_slice(&display_name(change));
            out.push(b'\n');
        }
    }

    if fmts.shortstat {
        let files = changes.len();
        let (insertions, deletions) = changes
            .iter()
            .filter_map(|c| c.counts)
            .fold((0u64, 0u64), |(i, d), (ci, cd)| {
                (i + u64::from(ci), d + u64::from(cd))
            });
        let mut line = format!(" {files} file{} changed", plural(files as u64));
        // git prints a zero count only when it would otherwise print neither.
        if insertions > 0 || deletions == 0 {
            line.push_str(&format!(
                ", {insertions} insertion{}(+)",
                plural(insertions)
            ));
        }
        if deletions > 0 || insertions == 0 {
            line.push_str(&format!(", {deletions} deletion{}(-)", plural(deletions)));
        }
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }

    if fmts.summary {
        for change in changes {
            match change.kind {
                ChangeKind::Added => {
                    append_mode_name(out, "create", change.new_mode, &change.path);
                }
                ChangeKind::Deleted => {
                    append_mode_name(out, "delete", change.old_mode, &change.path);
                }
                ChangeKind::Renamed | ChangeKind::Copied => {
                    let verb = if change.kind == ChangeKind::Renamed {
                        "rename"
                    } else {
                        "copy"
                    };
                    out.extend_from_slice(format!(" {verb} ").as_bytes());
                    out.extend_from_slice(&display_name(change));
                    out.extend_from_slice(format!(" ({}%)\n", change.score).as_bytes());
                    // git names the file only on a standalone mode change.
                    append_mode_change(out, change, None);
                }
                ChangeKind::Modified => {
                    append_mode_change(out, change, Some(&change.path));
                }
            }
        }
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// ` create mode 100644 <path>`, git's `show_file_mode_name()`.
fn append_mode_name(out: &mut Vec<u8>, verb: &str, mode: Option<u16>, path: &[u8]) {
    match mode {
        Some(mode) => out.extend_from_slice(format!(" {verb} mode {mode:06o} ").as_bytes()),
        None => out.extend_from_slice(format!(" {verb} ").as_bytes()),
    }
    out.extend_from_slice(&crate::quote::quoted_name_bytes(path));
    out.push(b'\n');
}

/// git's `show_mode_change()`: only when both sides have a mode and they differ.
fn append_mode_change(out: &mut Vec<u8>, change: &FileChange, name: Option<&[u8]>) {
    let (Some(old), Some(new)) = (change.old_mode, change.new_mode) else {
        return;
    };
    if old == new {
        return;
    }
    out.extend_from_slice(format!(" mode change {old:06o} => {new:06o}").as_bytes());
    if let Some(name) = name {
        out.push(b' ');
        out.extend_from_slice(&crate::quote::quoted_name_bytes(name));
    }
    out.push(b'\n');
}

/// The name a change is shown under: the compacted `a => b` form for a rename or
/// copy, the quoted path otherwise.
fn display_name(change: &FileChange) -> Vec<u8> {
    match &change.source {
        Some(source) => super::diff_pairs::pprint_rename(source, &change.path),
        None => crate::quote::quoted_name_bytes(&change.path),
    }
}


// ---------------------------------------------------------------------------
// local timezone
// ---------------------------------------------------------------------------

/// The UTC offset in seconds that `$TZ` (or `/etc/localtime`) prescribes for the
/// instant `seconds`, which is what `--date=…-local` renders in. Zero when no
/// timezone database can be read, which is also the right answer for UTC.
fn local_offset(seconds: i64) -> i32 {
    static ZONE: std::sync::OnceLock<Option<Zone>> = std::sync::OnceLock::new();
    ZONE.get_or_init(load_zone)
        .as_ref()
        .map_or(0, |zone| zone.offset_at(seconds))
}

/// The parts of a TZif file that matter for formatting a timestamp.
struct Zone {
    /// `(transition instant, index into `types`)`, ascending.
    transitions: Vec<(i64, usize)>,
    /// `(UTC offset in seconds, is_dst)` per local time type.
    types: Vec<(i32, bool)>,
}

impl Zone {
    fn offset_at(&self, seconds: i64) -> i32 {
        let index = match self
            .transitions
            .binary_search_by_key(&seconds, |&(when, _)| when)
        {
            Ok(i) => self.transitions[i].1,
            // Before the first transition RFC 8536 prescribes the first
            // non-DST type, falling back to the first type of any kind.
            Err(0) => {
                return self
                    .types
                    .iter()
                    .find(|&&(_, dst)| !dst)
                    .or_else(|| self.types.first())
                    .map_or(0, |&(offset, _)| offset);
            }
            Err(i) => self.transitions[i - 1].1,
        };
        self.types.get(index).map_or(0, |&(offset, _)| offset)
    }
}

/// Resolve `$TZ` the way libc does and parse the TZif file it names.
fn load_zone() -> Option<Zone> {
    let tz = std::env::var("TZ").unwrap_or_default();
    let tz = tz.strip_prefix(':').unwrap_or(&tz);

    let mut candidates: Vec<PathBuf> = Vec::new();
    if tz.is_empty() {
        candidates.push(PathBuf::from("/etc/localtime"));
    } else if tz.starts_with('/') {
        candidates.push(PathBuf::from(tz));
    } else if !tz.split('/').any(|part| part == ".." || part.is_empty()) {
        for root in [
            "/usr/share/zoneinfo",
            "/var/db/timezone/zoneinfo",
            "/etc/zoneinfo",
        ] {
            candidates.push(Path::new(root).join(tz));
        }
    }

    for path in candidates {
        if let Some(zone) = std::fs::read(&path).ok().as_deref().and_then(parse_tzif) {
            return Some(zone);
        }
    }
    // No file matched. A bare POSIX `<name><offset>` string still has an answer.
    posix_zone(tz)
}

/// A POSIX `TZ` string with no DST rule, e.g. `UTC0` or `EST5`. The POSIX offset
/// counts west of Greenwich, the opposite of the sign every other layer uses.
fn posix_zone(tz: &str) -> Option<Zone> {
    let rest = tz.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    if rest.is_empty() && tz.is_empty() {
        return None;
    }
    let (sign, digits) = match rest.strip_prefix('-') {
        Some(d) => (-1i32, d),
        None => (1i32, rest.strip_prefix('+').unwrap_or(rest)),
    };
    let mut parts = digits.split(':');
    let hours: i32 = parts.next()?.parse().ok()?;
    let minutes: i32 = parts.next().map_or(Ok(0), str::parse).ok()?;
    let secs: i32 = parts.next().map_or(Ok(0), str::parse).ok()?;
    if parts.next().is_some() {
        return None;
    }
    let west = sign * (hours * 3600 + minutes * 60 + secs);
    Some(Zone {
        transitions: Vec::new(),
        types: vec![(-west, false)],
    })
}

/// Header counts of a TZif block, in file order.
struct TzCounts {
    isutcnt: usize,
    isstdcnt: usize,
    leapcnt: usize,
    timecnt: usize,
    typecnt: usize,
    charcnt: usize,
}

/// Parse a TZif file (RFC 8536), preferring the 64-bit block of a v2+ file.
fn parse_tzif(data: &[u8]) -> Option<Zone> {
    if data.get(..4)? != b"TZif" {
        return None;
    }
    let version = *data.get(4)?;
    let mut pos = 20;
    let counts = read_counts(data, &mut pos)?;

    if version >= b'2' {
        // Skip the legacy 32-bit block and the second header it precedes.
        pos = pos.checked_add(block_len(&counts, 4)?)?;
        if data.get(pos..pos.checked_add(4)?)? != b"TZif" {
            return None;
        }
        pos = pos.checked_add(20)?;
        let counts = read_counts(data, &mut pos)?;
        read_block(data, pos, &counts, 8)
    } else {
        read_block(data, pos, &counts, 4)
    }
}

fn read_counts(data: &[u8], pos: &mut usize) -> Option<TzCounts> {
    let mut next = || -> Option<usize> {
        let raw: [u8; 4] = data.get(*pos..*pos + 4)?.try_into().ok()?;
        *pos += 4;
        Some(u32::from_be_bytes(raw) as usize)
    };
    Some(TzCounts {
        isutcnt: next()?,
        isstdcnt: next()?,
        leapcnt: next()?,
        timecnt: next()?,
        typecnt: next()?,
        charcnt: next()?,
    })
}

/// The byte length of a data block whose transition times are `time_size` wide.
fn block_len(counts: &TzCounts, time_size: usize) -> Option<usize> {
    counts
        .timecnt
        .checked_mul(time_size + 1)?
        .checked_add(counts.typecnt.checked_mul(6)?)?
        .checked_add(counts.charcnt)?
        .checked_add(counts.leapcnt.checked_mul(time_size + 4)?)?
        .checked_add(counts.isstdcnt)?
        .checked_add(counts.isutcnt)
}

fn read_block(data: &[u8], mut pos: usize, counts: &TzCounts, time_size: usize) -> Option<Zone> {
    let mut times: Vec<i64> = Vec::with_capacity(counts.timecnt);
    for _ in 0..counts.timecnt {
        let raw = data.get(pos..pos.checked_add(time_size)?)?;
        times.push(match time_size {
            8 => i64::from_be_bytes(raw.try_into().ok()?),
            _ => i64::from(i32::from_be_bytes(raw.try_into().ok()?)),
        });
        pos += time_size;
    }
    let indices = data.get(pos..pos.checked_add(counts.timecnt)?)?.to_vec();
    pos += counts.timecnt;

    let mut types: Vec<(i32, bool)> = Vec::with_capacity(counts.typecnt);
    for _ in 0..counts.typecnt {
        let raw = data.get(pos..pos.checked_add(6)?)?;
        let offset = i32::from_be_bytes(raw[..4].try_into().ok()?);
        types.push((offset, raw[4] != 0));
        pos += 6;
    }
    if types.is_empty() {
        return None;
    }

    let transitions = times
        .into_iter()
        .zip(indices)
        .map(|(when, index)| (when, usize::from(index)))
        .collect();
    Some(Zone { transitions, types })
}

// ---------------------------------------------------------------------------
// list / exists
// ---------------------------------------------------------------------------

/// `parse_options()` over an `OPT_END()`-only table with no flags — the shape
/// `list`, `exists` and `write` share.
///
/// Everything dashed is `PARSE_OPT_UNKNOWN` (reported with its `=<value>` intact
/// and the block on stderr at 129) except the two help spellings, which reach the
/// same block on stdout. `--` and `--end-of-options` end the scan and are dropped,
/// leaving what follows for the caller to count.
fn scan_no_options<'a>(
    args: &'a [String],
    usage: &str,
) -> std::result::Result<Vec<&'a String>, ExitCode> {
    let mut operands = Vec::new();
    let mut literal = false;
    for a in args {
        if literal {
            operands.push(a);
            continue;
        }
        match a.as_str() {
            "--" | "--end-of-options" => literal = true,
            s if super::asks_for_help(s, "") => return Err(super::show_usage(usage)),
            s if s.starts_with('-') && s != "-" => {
                return Err(super::unknown_option(s, usage))
            }
            _ => operands.push(a),
        }
    }
    Ok(operands)
}

/// `git reflog list` — every ref under `$GIT_DIR/logs` that owns a log file.
fn list(repo: &gix::Repository, rest: &[String]) -> Result<ExitCode> {
    // `cmd_reflog_list` parses an empty table with no flags, so every dashed word
    // is `PARSE_OPT_UNKNOWN` and only then does the leftover count matter:
    // ``error(_("%s does not accept arguments: '%s'"), "list", argv[0])``, whose
    // -1 return reaches exit(3) as 255.
    match scan_no_options(rest, LIST_USAGE) {
        Err(code) => return Ok(code),
        Ok(operands) => {
            if let Some(a) = operands.first() {
                eprintln!("error: list does not accept arguments: '{a}'");
                return Ok(ExitCode::from(255));
            }
        }
    }
    if repo.git_dir() != repo.common_dir() {
        bail!("`reflog list` from a linked worktree is not supported");
    }

    let mut names: Vec<String> = Vec::new();
    collect_logs(&repo.git_dir().join("logs"), "", &mut names)?;

    let mut out = String::new();
    for name in names {
        out.push_str(&name);
        out.push('\n');
    }
    print!("{out}");
    Ok(ExitCode::SUCCESS)
}

/// `git reflog exists <ref>` — a literal test for `$GIT_DIR/logs/<ref>`.
fn exists(repo: &gix::Repository, rest: &[String]) -> Result<ExitCode> {
    let operands = match scan_no_options(rest, EXISTS_USAGE) {
        Ok(operands) => operands,
        Err(code) => return Ok(code),
    };
    // `if (!argc) usage_with_options(...)` — no `error:` line, and only the
    // *first* operand is read, so a second one is ignored rather than refused.
    let Some(name) = operands.first() else {
        eprint!("{EXISTS_USAGE}");
        return Ok(ExitCode::from(129));
    };

    // git validates with REFNAME_ALLOW_ONELEVEL, i.e. `master` is well-formed
    // even though it is not a full ref name — that is gitoxide's partial name.
    if <&gix::refs::PartialNameRef>::try_from(name.as_str()).is_err() {
        eprintln!("fatal: invalid ref format: {name}");
        return Ok(ExitCode::from(128));
    }

    let present = reflog_roots(repo)
        .iter()
        .any(|root| root.join(name).is_file());
    Ok(if present {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

/// Emit git's "unknown revision" fatal block verbatim and return its exit code.
fn fatal_ambiguous(spec: &str) -> u8 {
    eprintln!(
        "fatal: ambiguous argument '{spec}': unknown revision or path not in the working tree."
    );
    eprintln!("Use '--' to separate paths from revisions, like this:");
    eprintln!("'git <command> [<revision>...] -- [<file>...]'");
    128
}

enum Selector<'a> {
    Index(usize),
    Date(&'a str),
}

/// Split `<ref>@{<selector>}` into the ref name as typed and its selector.
/// A spec without a trailing `@{...}` yields `(spec, None)`.
fn split_selector(spec: &str) -> (&str, Option<Selector<'_>>) {
    let b = spec.as_bytes();
    if b.len() < 4 || b[b.len() - 1] != b'}' {
        return (spec, None);
    }
    // ```c
    // for (at = len-4; at >= 0; at--) {
    //         if (str[at] == '@' && str[at+1] == '{') {
    //                 if (str[at+2] == '-') { … nth_prior = 1; continue; }
    //                 if (!upstream_mark(str + at, len - at) && !push_mark(str + at, len - at)) {
    //                         reflog_len = (len-1) - (at+2);
    //                         len = at;
    //                 }
    //                 break;
    //         }
    // }
    // ```
    // (`object-name.c:705-724`). `at` may be 0 — a bare `@{…}` is HEAD's own log,
    // not a ref named `@` — and a `-` selector keeps the scan going leftwards
    // because `@{-<n>}` is `interpret_nth_prior_checkout()`'s, not a reflog's.
    let mut open = None;
    for at in (0..=b.len() - 4).rev() {
        if b[at] != b'@' || b[at + 1] != b'{' {
            continue;
        }
        if b[at + 2] == b'-' {
            continue;
        }
        let rest = &spec[at..];
        let is_mark = ["@{upstream}", "@{u}", "@{push}"]
            .iter()
            .any(|m| rest.len() >= m.len() && rest[..m.len()].eq_ignore_ascii_case(m));
        if !is_mark {
            open = Some(at);
        }
        break;
    }
    let open = match open {
        Some(at) => at,
        None => return (spec, None),
    };
    let inner = &spec[open + 2..spec.len() - 1];
    // ```c
    // for (i = nth = 0; 0 <= nth && i < reflog_len; i++) {
    //         char ch = str[at+2+i];
    //         if ('0' <= ch && ch <= '9') nth = nth * 10 + ch - '0';
    //         else nth = -1;
    // }
    // if (100000000 <= nth) { at_time = nth; nth = -1; }
    // ```
    // An all-digit run large enough to be a unix timestamp is a *date*, not an
    // ordinal — `main@{100000000}` asks for the log as it stood in March 1973.
    let digits = !inner.is_empty() && inner.bytes().all(|c| c.is_ascii_digit());
    match inner.parse::<u64>() {
        Ok(n) if digits && n < 100_000_000 => (&spec[..open], Some(Selector::Index(n as usize))),
        _ => (&spec[..open], Some(Selector::Date(inner))),
    }
}

/// Abbreviate `id` according to the `--abbrev` family of options.
fn abbrev_id(repo: &gix::Repository, id: ObjectId, abbrev: &Abbrev, fallback_len: usize) -> String {
    match abbrev {
        Abbrev::Full => id.to_string(),
        Abbrev::Len(n) => id.to_hex_with_len(*n).to_string(),
        Abbrev::Auto => short_id(repo, id, fallback_len),
    }
}

/// Abbreviate `id` the way git does by default: the shortest unique prefix at
/// least `core.abbrev` long. Falls back to a plain `core.abbrev`-length prefix
/// when the object is missing from the odb.
fn short_id(repo: &gix::Repository, id: ObjectId, fallback_len: usize) -> String {
    match id.attach(repo).shorten() {
        Ok(prefix) => prefix.to_string(),
        Err(_) => id.to_hex_with_len(fallback_len).to_string(),
    }
}

/// The configured abbreviation length: `core.abbrev` when set to a number, the
/// full hash for `no`/`false`, otherwise git's automatic length derived from the
/// packed object count (`max(7, ceil(bits(count) / 2))`).
fn abbrev_len(repo: &gix::Repository, full: usize) -> usize {
    if let Some(value) = repo.config_snapshot().string("core.abbrev") {
        match value.to_str_lossy().as_ref() {
            "no" | "false" => return full,
            "auto" => {}
            n => {
                if let Ok(n) = n.parse::<usize>() {
                    return n.clamp(4, full);
                }
            }
        }
    }
    let count = repo.objects.packed_object_count().unwrap_or(0);
    let len = (64 - count.leading_zeros()).div_ceil(2) as usize;
    len.max(7).min(full)
}

/// The directories that hold reflog files. Normally one; a linked worktree keeps
/// its per-worktree logs (`HEAD`, `refs/bisect/*`) beside the shared ones.
fn reflog_roots(repo: &gix::Repository) -> Vec<PathBuf> {
    let git = repo.git_dir().join("logs");
    let common = repo.common_dir().join("logs");
    if git == common {
        vec![git]
    } else {
        vec![git, common]
    }
}

/// Append every log file below `dir` to `out` as a `/`-joined ref name, sorting
/// each directory's entries by name so the result matches git's tree walk (a
/// sub-directory is descended at its own sort position, not after its siblings).
fn collect_logs(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    let mut items: Vec<(String, bool)> = Vec::new();
    for entry in read {
        let entry = entry?;
        let is_dir = entry.file_type()?.is_dir();
        items.push((entry.file_name().to_string_lossy().into_owned(), is_dir));
    }
    items.sort();

    for (name, is_dir) in items {
        let full = format!("{prefix}{name}");
        if is_dir {
            collect_logs(&dir.join(&name), &format!("{full}/"), out)?;
        } else {
            out.push(full);
        }
    }
    Ok(())
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// delete / expire — the reflog write path
// ---------------------------------------------------------------------------

/// One line of a reflog file, kept whole so a rewrite is byte-exact.
///
/// git rewrites these files as text: `<old> <new> <who> <ts> <tz>\t<message>`. The two
/// ids and the timestamp are the only fields the write path reasons about, so the rest
/// of the line is carried through untouched.
struct RawLine {
    old: ObjectId,
    new: ObjectId,
    time: i64,
    bytes: Vec<u8>,
}

/// Parse one reflog line. `None` for a line git would not have written.
fn parse_raw_line(line: &[u8]) -> Option<RawLine> {
    let hexsz = line.iter().position(|b| *b == b' ')?;
    let old = ObjectId::from_hex(&line[..hexsz]).ok()?;
    let rest = &line[hexsz + 1..];
    let new_end = rest.iter().position(|b| *b == b' ')?;
    let new = ObjectId::from_hex(&rest[..new_end]).ok()?;
    // The committer block ends at the timestamp, which is the second-to-last
    // whitespace-separated field before the tab (`<name> <email> <secs> <tz>`).
    let head = match rest.iter().position(|b| *b == b'\t') {
        Some(tab) => &rest[..tab],
        None => rest,
    };
    let mut fields = head.rsplit(|b| *b == b' ');
    let _tz = fields.next()?;
    let secs = fields.next()?;
    let time: i64 = std::str::from_utf8(secs).ok()?.parse().ok()?;
    Some(RawLine {
        old,
        new,
        time,
        bytes: line.to_vec(),
    })
}

/// The `message` half of a reflog line, which is what
/// `should_expire_reflog_ent_verbose()` prints.
///
/// git's `message` still carries the line's own newline (the reflog file's
/// records are newline-terminated and it prints `"%s"` with no separator);
/// [`read_raw_log`] strips it, so it is put back here.
fn raw_line_message(line: &RawLine) -> Vec<u8> {
    let mut out = match line.bytes.iter().position(|b| *b == b'\t') {
        Some(tab) => line.bytes[tab + 1..].to_vec(),
        None => Vec::new(),
    };
    out.push(b'\n');
    out
}

/// `reflog_expire_config()` (reflog.c:35-83): the `gc.reflogExpire` /
/// `gc.reflogExpireUnreachable` defaults plus the `gc.<pattern>.reflog*` entries.
struct ExpireConfig {
    /// `opts->default_expire_total`.
    default_total: i64,
    /// `opts->default_expire_unreachable`.
    default_unreachable: i64,
    /// `opts->entries`, in configuration order — the first matching pattern wins.
    entries: Vec<(String, Option<i64>, Option<i64>)>,
}

impl ExpireConfig {
    fn read(repo: &gix::Repository, default_total: i64, default_unreachable: i64) -> Self {
        let mut out = ExpireConfig {
            default_total,
            default_unreachable,
            entries: Vec::new(),
        };
        let config = repo.config_snapshot();
        let Some(sections) = config.sections_by_name("gc") else {
            return out;
        };
        for section in sections {
            // `parse_config_key(var, "gc", &pattern, &pattern_len, &key)`: the subsection
            // is the pattern, and its absence names the defaults.
            let pattern = section
                .header()
                .subsection_name()
                .map(|s| s.to_str_lossy().into_owned());
            for (name, value) in [
                ("reflogExpire", REFLOG_EXPIRE_TOTAL),
                ("reflogExpireUnreachable", REFLOG_EXPIRE_UNREACH),
            ] {
                let Some(raw) = section.value(name) else { continue };
                // `git_config_expiry_date()` is `parse_expiry_date()` again.
                // `git_config_expiry_date()` failing makes `reflog_expire_config()`
                // return -1, which `repo_config()` reports through its own
                // `die()`; a value this port cannot read is left to the defaults
                // rather than invented.
                let Some(when) = expiry_date(&raw.to_str_lossy()) else {
                    continue;
                };
                match &pattern {
                    None => match value {
                        REFLOG_EXPIRE_TOTAL => out.default_total = when,
                        _ => out.default_unreachable = when,
                    },
                    Some(pattern) => {
                        let slot = match out.entries.iter().position(|(p, _, _)| p == pattern) {
                            Some(at) => at,
                            None => {
                                out.entries.push((pattern.clone(), None, None));
                                out.entries.len() - 1
                            }
                        };
                        match value {
                            REFLOG_EXPIRE_TOTAL => out.entries[slot].1 = Some(when),
                            _ => out.entries[slot].2 = Some(when),
                        }
                    }
                }
            }
        }
        out
    }

    /// `reflog_expire_options_set_refname()` (reflog.c:99-133) for one ref: what the
    /// command line did not pin is filled from the first matching pattern, from
    /// `refs/stash`'s never-expire rule, or from the defaults.
    fn for_ref(&self, refname: &str, cli_total: Option<i64>, cli_unreach: Option<i64>) -> (i64, i64) {
        if let (Some(total), Some(unreach)) = (cli_total, cli_unreach) {
            return (total, unreach);
        }
        let (total, unreach) = match self
            .entries
            .iter()
            .find(|(pattern, _, _)| glob_matches(pattern, refname))
        {
            Some((_, total, unreach)) => (total.unwrap_or(0), unreach.unwrap_or(0)),
            // `if (!strcmp(ref, "refs/stash")) { … = 0; … = 0; return; }` — the stash log
            // never expires unless the caller says otherwise.
            None if refname == "refs/stash" => (0, 0),
            None => (self.default_total, self.default_unreachable),
        };
        (cli_total.unwrap_or(total), cli_unreach.unwrap_or(unreach))
    }
}

/// `REFLOG_EXPIRE_TOTAL` / `REFLOG_EXPIRE_UNREACH`, as the two slots
/// `reflog_expire_config()` writes.
const REFLOG_EXPIRE_TOTAL: u8 = 1;
const REFLOG_EXPIRE_UNREACH: u8 = 2;

/// `parse_expiry_date()` (date.c) for a configuration value.
fn expiry_date(value: &str) -> Option<i64> {
    match value {
        "now" | "all" => Some(i64::MAX),
        "never" | "false" => Some(0),
        _ => {
            let (timestamp, error) = crate::date::approxidate_careful(value);
            (!error).then_some(timestamp)
        }
    }
}

/// `wildmatch(ent->pattern, ref, 0)`.
fn glob_matches(pattern: &str, refname: &str) -> bool {
    gix::glob::wildmatch(
        pattern.into(),
        refname.into(),
        gix::glob::wildmatch::Mode::empty(),
    )
}

/// The per-worktree `logs` directory of every linked worktree, with the id that
/// prefixes the ref names inside it.
///
/// A linked worktree's refs live at `$GIT_COMMON_DIR/worktrees/<id>/logs/<ref>`
/// and are named `worktrees/<id>/<ref>`, which is what `strbuf_worktree_ref()`
/// builds in `collect_reflog()`.
fn linked_worktree_log_roots(repo: &gix::Repository) -> Vec<(String, PathBuf)> {
    let dir = repo.common_dir().join("worktrees");
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = read
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            // The current worktree's own logs were already collected by name.
            (e.path() != repo.git_dir()).then(|| (id, e.path().join("logs")))
        })
        .collect();
    out.sort();
    out
}

/// The file a ref's reflog lives in. `HEAD` (and the other per-worktree
/// pseudo-refs) belong to this worktree; everything else is shared.
pub(crate) fn log_file(repo: &gix::Repository, full_name: &str) -> PathBuf {
    // `worktrees/<id>/<ref>` is another worktree's private ref, whose store is
    // `$GIT_COMMON_DIR/worktrees/<id>` — the `logs/` goes *inside* it, not in front.
    if let Some(rest) = full_name.strip_prefix("worktrees/") {
        if let Some((id, ref_name)) = rest.split_once('/') {
            return repo
                .common_dir()
                .join("worktrees")
                .join(id)
                .join("logs")
                .join(ref_name);
        }
    }
    let root = if full_name.starts_with("refs/") {
        repo.common_dir()
    } else {
        repo.git_dir()
    };
    root.join("logs").join(full_name)
}

/// The full ref name behind a selector's ref part, as `dwim_log()` resolves it.
fn resolve_log_ref(repo: &gix::Repository, name: &str) -> String {
    if name == "HEAD" {
        return name.to_owned();
    }
    match repo.try_find_reference(name).ok().flatten() {
        Some(r) => r.name().as_bstr().to_str_lossy().into_owned(),
        None => name.to_owned(),
    }
}

/// Read a reflog file as raw lines, oldest first. `None` when there is no log.
fn read_raw_log(path: &Path) -> Result<Option<Vec<RawLine>>> {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for line in data.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        match parse_raw_line(line) {
            Some(parsed) => out.push(parsed),
            None => crate::git_fatal!("bad reflog line in {}", path.display()),
        }
    }
    Ok(Some(out))
}

/// Write a reflog file back from its surviving lines. An empty survivor set leaves an
/// empty file rather than removing it, which is what `git reflog expire` leaves behind.
fn write_raw_log(path: &Path, lines: &[RawLine], rewrite: bool) -> Result<()> {
    let mut out: Vec<u8> = Vec::new();
    let mut previous: Option<ObjectId> = None;
    for line in lines {
        // `--rewrite`: a survivor whose predecessor was dropped starts from what is now
        // the previous entry's new id, so the chain reads continuously again.
        if rewrite && previous.is_some_and(|p| p != line.old) {
            let want = previous.expect("checked");
            let mut fixed = want.to_hex().to_string().into_bytes();
            fixed.extend_from_slice(&line.bytes[want.to_hex().to_string().len()..]);
            out.extend_from_slice(&fixed);
        } else {
            out.extend_from_slice(&line.bytes);
        }
        out.push(b'\n');
        previous = Some(line.new);
    }
    std::fs::write(path, out)?;
    Ok(())
}

/// `git reflog delete [--rewrite] [--updateref] [--dry-run] <ref>@{<n>}…` — port of
/// `cmd_reflog_delete`.
///
/// Each selector names one entry, counted from the newest. The entry is dropped and the
/// rest of the file is left as it was: the neighbours keep the ids they recorded unless
/// `--rewrite` asks for the chain to be closed up, and the ref itself only moves under
/// `--updateref`. A selector past the end of the log is silently ignored, as git's
/// `mark_reflog_expiry` is.
fn delete_entries(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut rewrite = false;
    let mut updateref = false;
    let mut dry_run = false;
    let mut verbose = false;
    let mut selectors: Vec<&str> = Vec::new();
    let mut literal = false;
    for a in args {
        if literal {
            selectors.push(a);
            continue;
        }
        match a.as_str() {
            "--" | "--end-of-options" => literal = true,
            // `-n` is the only short entry in the table, so it is what
            // `parse_short_opt()` consumes before the `h` test.
            s if super::asks_for_help(s, "n") => return Ok(super::show_usage(DELETE_USAGE)),
            "--rewrite" => rewrite = true,
            "--updateref" => updateref = true,
            "-n" | "--dry-run" => dry_run = true,
            "--verbose" => verbose = true,
            s if s.starts_with('-') && s != "-" => {
                return Ok(super::unknown_option(s, DELETE_USAGE))
            }
            s => selectors.push(s),
        }
    }
    if selectors.is_empty() {
        // `return error(_("no reflog specified to delete"))` — a bare `error()`,
        // so no usage block, and its -1 reaches exit(3) as 255.
        eprintln!("error: no reflog specified to delete");
        return Ok(ExitCode::from(255));
    }

    // `for (i = 0; i < argc; i++) status |= reflog_delete(argv[i], flags, verbose);`
    // (`builtin/reflog.c:328-329`): every operand is attempted, and one failure
    // only decides the exit status.
    let mut status = ExitCode::SUCCESS;
    for spec in selectors {
        if !delete_one(repo, spec, rewrite, updateref, dry_run, verbose)? {
            status = ExitCode::from(255);
        }
    }
    Ok(status)
}

/// `reflog_delete()` (`reflog.c:520-566`) for one `<ref>@{<selector>}` operand.
/// `false` is its `error()` return.
///
/// ```c
/// const char *spec = strstr(rev, "@{");
/// if (!spec)
///         return error(_("not a reflog: %s"), rev);
/// if (!repo_dwim_log(the_repository, rev, spec - rev, NULL, &ref)) {
///         status |= error(_("no reflog for '%s'"), rev);
///         goto cleanup;
/// }
/// recno = strtoul(spec + 2, &ep, 10);
/// if (*ep == '}') {
///         opts.recno = -recno;
///         refs_for_each_reflog_ent(refs, ref, count_reflog_ent, &opts);
/// } else {
///         opts.expire_total = approxidate(spec + 2);
///         refs_for_each_reflog_ent(refs, ref, count_reflog_ent, &opts);
///         opts.expire_total = 0;
/// }
/// status |= refs_reflog_expire(refs, ref, flags, …, should_prune_fn, …, &cb);
/// ```
///
/// Three things a re-derivation gets wrong. The lookup is `repo_dwim_log()`, so
/// an ambiguous `dup@{0}` finds `refs/heads/dup`'s log rather than failing. The
/// message names the operand *as typed*, selector included. And the selector may
/// be a date: `count_reflog_ent()` then counts the entries older than it and
/// `expire_total` is reset to 0, which turns the date into an ordinal before the
/// expiry walk ever runs.
fn delete_one(
    repo: &gix::Repository,
    spec: &str,
    rewrite: bool,
    updateref: bool,
    dry_run: bool,
    verbose: bool,
) -> Result<bool> {
    let Some(at) = spec.find("@{") else {
        eprintln!("error: not a reflog: {spec}");
        return Ok(false);
    };
    let Some(full) = dwim_log(repo, &spec[..at]) else {
        eprintln!("error: no reflog for '{spec}'");
        return Ok(false);
    };
    let path = log_file(repo, &full);
    let mut lines = read_raw_log(&path)?.unwrap_or_default();

    // `strtoul(spec + 2, &ep, 10)`: leading digits, and `*ep == '}'` is what says
    // the whole selector was the number.
    let tail = &spec[at + 2..];
    let digits = tail.len() - tail.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let mut countdown: i64 = if tail[digits..] == *"}" {
        // `opts.recno = -recno` then one `++` per entry.
        lines.len() as i64 - tail[..digits].parse::<i64>().unwrap_or(0)
    } else {
        // `if (!cb->expire_total || timestamp < cb->expire_total) cb->recno++;`
        let target = crate::date::approxidate(tail);
        lines.iter().filter(|l| l.time < target).count() as i64
    };

    // `refs_reflog_expire()` walks the log oldest entry first, and
    // `should_expire_reflog_ent()` reduces — with everything but `recno` unset —
    // to `if (cb->opts.recno && --(cb->opts.recno) == 0) return 1;`. So exactly one
    // entry is dropped, and a countdown that never reaches 0 drops none.
    let mut doomed: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let expire = countdown != 0 && {
            countdown -= 1;
            countdown == 0
        };
        if expire {
            doomed = Some(i);
        }
        if verbose {
            // `printf("keep %s", message)` — the reflog message already carries its
            // own newline, so git adds none.
            let verb = if !expire {
                "keep"
            } else if dry_run {
                "would prune"
            } else {
                "prune"
            };
            let message = match line.bytes.iter().position(|b| *b == b'\t') {
                Some(tab) => &line.bytes[tab + 1..],
                None => &[][..],
            };
            let mut out = format!("{verb} ").into_bytes();
            out.extend_from_slice(message);
            out.push(b'\n');
            std::io::Write::write_all(&mut std::io::stdout(), &out)?;
        }
    }

    if dry_run {
        return Ok(true);
    }
    if let Some(i) = doomed {
        lines.remove(i);
        write_raw_log(&path, &lines, rewrite)?;
    }
    // `EXPIRE_REFLOGS_UPDATE_REF`: the files backend writes the ref straight to its
    // lockfile, so the update leaves no reflog entry of its own.
    if updateref {
        if let Some(newest) = lines.last() {
            update_ref_to(repo, &full, newest.new)?;
        }
    }
    Ok(true)
}

/// Point `full_name` at `oid` without adding a reflog entry of its own, which is
/// what `--updateref` does after the log was rewritten.
///
/// The files backend does this inside the lock it already holds for the log:
///
/// ```c
/// if ((flags & EXPIRE_REFLOGS_UPDATE_REF) && !is_null_oid(&cb.last_kept_oid)) {
///         if (write_ref_to_lockfile(refs, &lock, &cb.last_kept_oid, 0, &err) ||
///             commit_ref(&lock)) { … }
/// }
/// ```
/// (`refs/files-backend.c`)
///
/// `write_ref_to_lockfile()`/`commit_ref()` sit *below* the transaction layer, so
/// no reflog entry is appended. A `gix` `RefEdit` cannot express that — `RefLog::Only`
/// writes the log and not the ref (the exact inverse of what is wanted, which is
/// what this used to do), and `RefLog::AndReference` appends an entry whenever a log
/// file already exists, which here it always does. So the loose ref is written
/// directly, lock file and all.
fn update_ref_to(repo: &gix::Repository, full_name: &str, oid: ObjectId) -> Result<()> {
    let path = ref_file(repo, full_name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock = path.with_extension("lock");
    let mut body = oid.to_hex().to_string().into_bytes();
    body.push(b'\n');
    std::fs::write(&lock, &body)?;
    std::fs::rename(&lock, &path)?;
    Ok(())
}

/// The loose file a ref lives in, by the same per-worktree rule as [`log_file`].
fn ref_file(repo: &gix::Repository, full_name: &str) -> PathBuf {
    let root = if full_name.starts_with("refs/") {
        repo.common_dir()
    } else {
        repo.git_dir()
    };
    root.join(full_name)
}

/// `git reflog expire [--expire=<time>] [--expire-unreachable=<time>] [--all] …` — port
/// of `cmd_reflog_expire`.
///
/// An entry is dropped when it is older than the cutoff that applies to it: `--expire`
/// for one whose new id is still reachable from the ref, `--expire-unreachable` for one
/// whose is not. `now` expires everything, `never` nothing; without either option git's
/// `gc.reflogExpire` (90 days) and `gc.reflogExpireUnreachable` (30 days) defaults apply.
fn expire_entries(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    const DAY: i64 = 24 * 60 * 60;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut all = false;
    let mut single_worktree = false;
    let mut dry_run = false;
    let mut verbose = false;
    let mut rewrite = false;
    let mut updateref = false;
    let mut expire: Option<i64> = None;
    let mut expire_unreachable: Option<i64> = None;
    let mut refs: Vec<String> = Vec::new();
    // ```c
    // if (!strcmp(date, "never") || !strcmp(date, "false"))
    //         *timestamp = 0;
    // else if (!strcmp(date, "all") || !strcmp(date, "now"))
    //         *timestamp = TIME_MAX;
    // else
    //         *timestamp = approxidate_careful(date, &errors);
    // ```
    //
    // (`parse_expiry_date()`, date.c.) `never` is *zero*, not a floor — which matters
    // because `reflog_expiry_prepare()` compares the two cutoffs against each other.
    let cutoff = |value: &str| -> Option<i64> {
        match value {
            "now" | "all" => Some(i64::MAX),
            "never" | "false" => Some(0),
            _ => {
                let (timestamp, error) = crate::date::approxidate_careful(value);
                (!error).then_some(timestamp)
            }
        }
    };
    let mut literal = false;
    for a in args {
        let s = a.as_str();
        if literal {
            refs.push(s.to_owned());
            continue;
        }
        match s {
            "--" | "--end-of-options" => literal = true,
            // `-n` is `expire`'s only short entry, so it is what
            // `parse_short_opt()` consumes before the `h` test that answers help.
            _ if super::asks_for_help(s, "n") => return Ok(super::show_usage(EXPIRE_USAGE)),
            "--all" => all = true,
            "--single-worktree" => single_worktree = true,
            "-n" | "--dry-run" => dry_run = true,
            "--rewrite" => rewrite = true,
            "--updateref" => updateref = true,
            "--stale-fix" => {}
            "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            _ if s.starts_with("--expire-unreachable=") => {
                let v = &s["--expire-unreachable=".len()..];
                match cutoff(v) {
                    Some(t) => expire_unreachable = Some(t),
                    // `parse_opt_expiry_date_cb()` reports through
                    // `error(_("invalid timestamp '%s' given to '--%s'"), arg, opt->long_name)`
                    // (parse-options-cb.c), which `parse_options()` turns into exit 128
                    // via the `die` its callers install.
                    None => crate::git_fatal!(
                        "invalid timestamp '{v}' given to '--expire-unreachable'"
                    ),
                }
            }
            _ if s.starts_with("--expire=") => {
                let v = &s["--expire=".len()..];
                match cutoff(v) {
                    Some(t) => expire = Some(t),
                    None => crate::git_fatal!("invalid timestamp '{v}' given to '--expire'"),
                }
            }
            _ if s.starts_with('-') && s != "-" => {
                return Ok(super::unknown_option(s, EXPIRE_USAGE))
            }
            _ => refs.push(s.to_owned()),
        }
    }

    // `repo_config(the_repository, reflog_expire_config, &opts)` (builtin/reflog.c:216).
    let config = ExpireConfig::read(repo, now - 90 * DAY, now - 30 * DAY);

    let targets: Vec<String> = if all {
        // ```c
        // worktrees = get_worktrees();
        // for (p = worktrees; *p; p++) {
        //         if (single_worktree && !(*p)->is_current)
        //                 continue;
        //         collected.worktree = *p;
        //         refs_for_each_reflog(get_worktree_ref_store(*p), collect_reflog, &collected);
        // }
        // ```
        //
        // (builtin/reflog.c:253-260.) `--all` covers every worktree, not just this one —
        // a linked worktree contributes its per-worktree logs under
        // `worktrees/<id>/<ref>` (`collect_reflog()` drops the shared refs it would
        // otherwise report a second time).
        let mut names = Vec::new();
        for root in reflog_roots(repo) {
            collect_logs(&root, "", &mut names)?;
        }
        if !single_worktree {
            for (id, root) in linked_worktree_log_roots(repo) {
                let mut own = Vec::new();
                collect_logs(&root, "", &mut own)?;
                names.extend(
                    own.into_iter()
                        // The shared half of a linked worktree's store is the same
                        // `refs/…` this worktree already listed.
                        .filter(|name| !name.starts_with("refs/"))
                        .map(|name| format!("worktrees/{id}/{name}")),
                );
            }
        }
        names.sort();
        names.dedup();
        names
    } else if refs.is_empty() {
        // `cmd_reflog_expire` loops over `argc` refs and says nothing when there
        // are none: `git reflog expire` on its own is a successful no-op, not a
        // usage error.
        Vec::new()
    } else {
        refs.iter().map(|r| resolve_log_ref(repo, r)).collect()
    };

    for full in targets {
        // `reflog_expire_options_set_refname(&cb.opts, ref)` before each expiry: the
        // command line wins, then the first `gc.<pattern>.reflog*` whose pattern matches,
        // then `refs/stash`'s never-expire rule, then the `gc.reflog*` defaults.
        let (expire, expire_unreachable) = config.for_ref(&full, expire, expire_unreachable);
        let path = log_file(repo, &full);
        let Some(lines) = read_raw_log(&path)? else {
            continue;
        };
        // ```c
        // if (!cb->opts.expire_unreachable || is_head(refname)) {
        //         cb->unreachable_expire_kind = UE_HEAD;
        // } else {
        //         commit = lookup_commit_reference_gently(the_repository, oid, 1);
        //         …
        //         cb->unreachable_expire_kind = commit ? UE_NORMAL : UE_ALWAYS;
        // }
        //
        // if (cb->opts.expire_unreachable <= cb->opts.expire_total)
        //         cb->unreachable_expire_kind = UE_ALWAYS;
        //
        // switch (cb->unreachable_expire_kind) {
        // case UE_ALWAYS:  return;
        // case UE_HEAD:    refs_for_each_ref(…, push_tip_to_list, &cb->tips); …
        // case UE_NORMAL:  commit_list_insert(commit, &cb->mark_list);
        // }
        // ```
        //
        // (`reflog_expiry_prepare()`, reflog.c:446-483.) `HEAD`'s reachability set is
        // built from *every ref*, not from HEAD's own tip — which is what keeps a `HEAD`
        // entry naming a commit that some branch still holds. `UE_ALWAYS` skips the
        // reachability question entirely and expires on age alone.
        let kind = if expire_unreachable == 0 || is_head_log(&full) {
            Unreachable::Head
        } else if ref_tip_commit(repo, &full).is_some() {
            Unreachable::Normal
        } else {
            Unreachable::Always
        };
        let kind = match expire_unreachable <= expire {
            true => Unreachable::Always,
            false => kind,
        };
        let reachable = match kind {
            Unreachable::Always => None,
            Unreachable::Head => Some(reachable_from_all_refs(repo)?),
            Unreachable::Normal => Some(reachable_from_ref(repo, &full)?),
        };
        let mut kept: Vec<RawLine> = Vec::new();
        for line in lines {
            // `is_unreachable()` answers "keep" for a null id and for anything that is
            // not a commit, and it is asked about *both* ends of the entry.
            let unreachable = |id: &ObjectId| {
                reachable.as_ref().is_some_and(|set| {
                    !id.is_null() && repo.find_commit(*id).is_ok() && !set.contains(id)
                })
            };
            let expired = line.time < expire
                || (line.time < expire_unreachable
                    && match kind {
                        Unreachable::Always => true,
                        _ => unreachable(&line.old) || unreachable(&line.new),
                    });
            if verbose {
                // `should_expire_reflog_ent_verbose()` (reflog.c:404-424). `message`
                // carries its own newline.
                let what = match (expired, dry_run) {
                    (false, _) => "keep",
                    (true, true) => "would prune",
                    (true, false) => "prune",
                };
                let mut out = Vec::from(what.as_bytes());
                out.push(b' ');
                out.extend_from_slice(&raw_line_message(&line));
                let _ = std::io::Write::write_all(&mut std::io::stdout(), &out);
            }
            if !expired {
                kept.push(line);
            }
        }
        if dry_run {
            continue;
        }
        write_raw_log(&path, &kept, rewrite)?;
        if updateref {
            if let Some(newest) = kept.last() {
                update_ref_to(repo, &full, newest.new)?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// drop / write — whole reflogs, rather than entries within one
// ---------------------------------------------------------------------------

/// `cmd_reflog_drop`'s option table (builtin/reflog.c:358-363): two `OPT_BOOL`s,
/// so both negate and neither takes a value.
const DROP_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "all",             neg: true, arg: super::Arg::None },
    super::LongOpt { name: "single-worktree", neg: true, arg: super::Arg::None },
];

/// `git reflog drop [--all [--single-worktree]] [<ref>...]` — port of
/// `cmd_reflog_drop`, which removes whole reflogs rather than entries inside one.
///
/// A named ref goes through `repo_dwim_log()`, so the reflog has to belong to a ref
/// that resolves: a log file left behind by a ref that no longer exists is not found
/// by name. Each miss is an `error()` that does not stop the loop, and the `-1` it
/// ORs into the return reaches `exit(3)` as 255.
fn drop_reflogs(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut all = false;
    let mut single_worktree = false;
    let mut refs: Vec<&str> = Vec::new();
    let mut opts_done = false;
    for a in args {
        let s = a.as_str();
        if opts_done {
            refs.push(s);
            continue;
        }
        if s == "--" || s == "--end-of-options" {
            opts_done = true;
            continue;
        }
        // This table has no short entry at all, so the first character behind a
        // single `-` is the one `parse_short_opt()` tests for help.
        if super::asks_for_help(s, "") {
            return Ok(super::show_usage(DROP_USAGE));
        }
        if let Some(body) = s.strip_prefix("--") {
            // Resolved on the whole body, `=<value>` included, as `parse_long_opt()`
            // does — the lookup is what keeps `--all=x` from reaching the flag.
            let (opt, unset) = match super::resolve_long(DROP_OPTS, body) {
                super::Resolved::One(opt, unset) => (opt, unset),
                super::Resolved::Ambiguous(first, second) => {
                    return Ok(super::ambiguous_option(s, &first, &second, DROP_USAGE))
                }
                super::Resolved::Unknown => return Ok(super::unknown_option(s, DROP_USAGE)),
            };
            if body.contains('=') {
                // `PARSE_OPT_ERROR` out of `get_value()`: one line and no block,
                // naming the table entry however far it was abbreviated.
                let shown = if unset { format!("no-{}", opt.name) } else { opt.name.to_string() };
                eprintln!("error: option `{shown}' takes no value");
                return Ok(ExitCode::from(129));
            }
            match opt.name {
                "all" => all = !unset,
                "single-worktree" => single_worktree = !unset,
                _ => unreachable!("resolve_long only returns DROP_OPTS entries"),
            }
            continue;
        }
        if s.len() > 1 && s.starts_with('-') {
            return Ok(super::unknown_option(s, DROP_USAGE));
        }
        refs.push(s);
    }

    if !refs.is_empty() && all {
        // `usage(_("references specified along with --all"))` — the bare `usage()`,
        // which prints the string it was handed rather than the option block.
        eprintln!("usage: references specified along with --all");
        return Ok(ExitCode::from(129));
    }

    if all {
        // git collects from every worktree's ref store, or from this one alone under
        // `--single-worktree`, and deletes what it collected from the *main* store.
        // [`reflog_roots`] returns this worktree's private root and the shared one, so
        // both settings produce the same set here and the flag changes nothing: what
        // is missed is another worktree's per-worktree logs, which `--single-worktree`
        // would have excluded anyway. Kept as the same walk `expire --all` uses.
        let _ = single_worktree;
        let mut names = Vec::new();
        for root in reflog_roots(repo) {
            collect_logs(&root, "", &mut names)?;
        }
        names.sort();
        names.dedup();
        for name in names {
            remove_reflog_file(&log_file(repo, &name))?;
        }
        return Ok(ExitCode::SUCCESS);
    }

    let mut ret = ExitCode::SUCCESS;
    for name in refs {
        let Some(full) = dwim_log(repo, name) else {
            eprintln!("error: reflog could not be found: '{name}'");
            ret = ExitCode::from(255);
            continue;
        };
        remove_reflog_file(&log_file(repo, &full))?;
    }
    Ok(ret)
}

/// `repo_dwim_log()` (refs.c:840-879): the first of git's rev-parse spellings of
/// `name` that both resolves as a reference and has a reflog. When the reference
/// resolves somewhere else and only the target carries a log, that target is the
/// answer instead — which is how a symref's log is found through its own name.
pub(crate) fn dwim_log(repo: &gix::Repository, name: &str) -> Option<String> {
    let substituted = substitute_branch_name(repo, name);
    let name = substituted.as_deref().unwrap_or(name);
    // `ref_rev_parse_rules` (refs.c), in order.
    const RULES: &[&str] = &[
        "",
        "refs/",
        "refs/tags/",
        "refs/heads/",
        "refs/remotes/",
    ];
    let candidates = RULES
        .iter()
        .map(|prefix| format!("{prefix}{name}"))
        .chain(std::iter::once(format!("refs/remotes/{name}/HEAD")));
    for path in candidates {
        // `refs_resolve_ref_unsafe(refs, path.buf, RESOLVE_REF_READING, …)`: the
        // spelling has to name a reference that exists, not merely a log file, and a
        // symref chain that dead-ends counts as not existing.
        let Some(resolved) = resolve_ref_reading(repo, &path) else {
            continue;
        };
        if log_file(repo, &path).is_file() {
            return Some(path);
        }
        // `else if (strcmp(ref, path.buf) && refs_reflog_exists(refs, ref))`.
        if resolved != path && log_file(repo, &resolved).is_file() {
            return Some(resolved);
        }
    }
    None
}

/// `substitute_branch_name()` (refs.c:826-841): the name `repo_dwim_log()` really
/// looks up, when `repo_interpret_branch_name()` rewrites the whole spec. `None`
/// leaves the spec as typed.
///
/// Two of the three rewrites are here: the bare `@` that `interpret_empty_at()` turns
/// into `HEAD`, and `@{-<n>}`, which `interpret_nth_prior_checkout()` reads off HEAD's
/// own log. The third, `@{upstream}`/`@{push}`, is not — it resolves through the
/// branch's remote configuration and carries its own family of `die()`s, so
/// `git reflog drop @{u}` still reports the spec as a reflog it could not find rather
/// than git's `no upstream configured for branch '<name>'`.
fn substitute_branch_name(repo: &gix::Repository, name: &str) -> Option<String> {
    if name == "@" {
        return Some("HEAD".to_owned());
    }
    // The rewrite only applies when it consumed the entire spec; `@{-1}~2` keeps the
    // remainder, which is not a reflog name anyway.
    let (nth, used) = super::check_ref_format::parse_nth_prior(name.as_bytes())?;
    if used != name.len() {
        return None;
    }
    let branch = super::check_ref_format::nth_branch_switch(repo, nth)?;
    String::from_utf8(branch).ok()
}

/// `refs_resolve_ref_unsafe(…, RESOLVE_REF_READING, …)` — see
/// [`crate::refname::resolve_ref_reading`], which the ref-name shortening rules
/// need for the same reason `repo_dwim_log()` does.
pub(crate) use crate::refname::resolve_ref_reading;

/// `refs_delete_reflog()` for the files backend: unlink the log and then take the
/// directories it left empty, up to and including `logs` itself.
fn remove_reflog_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    }
    let mut dir = path.parent();
    while let Some(d) = dir {
        if std::fs::remove_dir(d).is_err() {
            break;
        }
        if d.file_name().is_some_and(|n| n == "logs") {
            break;
        }
        dir = d.parent();
    }
    Ok(())
}

/// `git reflog write <ref> <old-oid> <new-oid> <message>` — port of
/// `cmd_reflog_write`, which appends one entry to a reflog and touches nothing else:
/// the reference itself is neither created nor moved, and the two ids are recorded
/// exactly as given rather than read off the ref.
fn write_reflog(repo: &mut gix::Repository, args: &[String]) -> Result<ExitCode> {
    // The table is `OPT_END()` alone, so every dashed token is unknown — except the
    // help test, which `parse_options_step()` makes before it consults the table.
    let mut operands: Vec<&str> = Vec::new();
    let mut opts_done = false;
    for a in args {
        let s = a.as_str();
        if opts_done {
            operands.push(s);
            continue;
        }
        if s == "--" || s == "--end-of-options" {
            opts_done = true;
            continue;
        }
        if super::asks_for_help(s, "") {
            return Ok(super::show_usage(WRITE_USAGE));
        }
        if s.len() > 1 && s.starts_with('-') {
            return Ok(super::unknown_option(s, WRITE_USAGE));
        }
        operands.push(s);
    }
    // `usage_with_options()`, which unlike `-h` writes to stderr.
    if operands.len() != 4 {
        eprint!("{WRITE_USAGE}");
        return Ok(ExitCode::from(129));
    }
    let (name, old_spec, new_spec, message) = (operands[0], operands[1], operands[2], operands[3]);

    if !is_root_ref(name) && !super::check_ref_format::check_refname_format(name.as_bytes(), 0) {
        crate::git_fatal!("invalid reference name: {name}");
    }

    let old = parse_write_oid(repo, old_spec, "old")?;
    let new = parse_write_oid(repo, new_spec, "new")?;

    // `git_committer_info(0)`: `<name> <<email>> <seconds> <tz>`, the same string the
    // reflog writer would have filled in on its own. The non-strict form, so a machine
    // with no `user.*` gets the synthesized identity rather than a refusal.
    crate::ensure_reflog_identity(repo);
    let committer = repo
        .committer()
        .transpose()?
        .ok_or_else(|| anyhow!("no committer identity available for the reflog entry"))?;
    let mut line = format!(
        "{old} {new} {} <{}> {}",
        committer.name.to_str_lossy(),
        committer.email.to_str_lossy(),
        committer.time,
    )
    .into_bytes();
    // `log_ref_write_fd()` adds the tab and the message only when the normalized
    // message has something in it.
    let message = normalize_reflog_message(message);
    if !message.is_empty() {
        line.push(b'\t');
        line.extend_from_slice(message.as_bytes());
    }
    line.push(b'\n');

    // `ref_transaction_commit()` locks the ref before it appends, so a name that
    // collides with the reference namespace is refused there rather than by the file
    // system. `refs_verify_refname_available()` reports the *other* ref by name.
    if let Some(other) = df_conflicting_ref(repo, name)? {
        crate::git_fatal!(
            "cannot commit reflog update: cannot lock ref '{name}': \
             '{other}' exists; cannot create '{name}'"
        );
    }
    let path = log_file(repo, name);
    // With no ref in the way it can still be the *logs* that collide: a directory
    // where this entry's file belongs holds some other ref's log.
    if path.is_dir() {
        crate::git_fatal!(
            "cannot commit reflog update: cannot update the ref '{name}': \
             there are still logs under '{}'",
            crate::setup::git_path_display(repo, &path)
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(&line)?;
    Ok(ExitCode::SUCCESS)
}

/// `refs_verify_refname_available()`: the existing reference that stops `name` from
/// being a reference too, because one of them would have to be a directory the other
/// is a file in. git checks the prefixes of `name` first and then the names under it.
fn df_conflicting_ref(repo: &gix::Repository, name: &str) -> Result<Option<String>> {
    let mut names: Vec<String> = Vec::new();
    for reference in repo.references()?.all()?.filter_map(Result::ok) {
        names.push(reference.name().as_bstr().to_str_lossy().into_owned());
    }
    names.sort();
    let mut prefix_end = 0;
    while let Some(slash) = name[prefix_end..].find('/') {
        prefix_end += slash;
        let prefix = &name[..prefix_end];
        if names.iter().any(|n| n == prefix) {
            return Ok(Some(prefix.to_owned()));
        }
        prefix_end += 1;
    }
    let under = format!("{name}/");
    Ok(names.into_iter().find(|n| n.starts_with(&under)))
}

/// One of `reflog write`'s two object arguments: a full hex id, which must name an
/// object that is present unless it is the null id.
fn parse_write_oid(repo: &gix::Repository, spec: &str, which: &str) -> Result<ObjectId> {
    let Ok(id) = ObjectId::from_hex(spec.as_bytes()) else {
        crate::git_fatal!("invalid {which} object ID: '{spec}'");
    };
    if !id.is_null() && !repo.has_object(id) {
        crate::git_fatal!("{which} object '{spec}' does not exist");
    }
    Ok(id)
}

/// `is_root_ref()` (refs.c:915-939): an all-upper-case-`-`-`_` name that is not one
/// of the two pseudo-refs, and then either ends in `_HEAD` or is on the short list of
/// irregular root refs.
fn is_root_ref(name: &str) -> bool {
    let syntax_ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b == b'-' || b == b'_');
    if !syntax_ok || matches!(name, "FETCH_HEAD" | "MERGE_HEAD") {
        return false;
    }
    name.ends_with("_HEAD")
        || matches!(
            name,
            "HEAD"
                | "AUTO_MERGE"
                | "BISECT_EXPECTED_REV"
                | "NOTES_MERGE_PARTIAL"
                | "NOTES_MERGE_REF"
                | "MERGE_AUTOSTASH"
        )
}

/// `copy_reflog_msg()` (refs.c:1031-1045): every run of whitespace becomes one space,
/// a leading run is dropped outright, and the result is right-trimmed.
fn normalize_reflog_message(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut was_space = true;
    for c in msg.chars() {
        let is_space = c.is_ascii_whitespace();
        if was_space && is_space {
            continue;
        }
        was_space = is_space;
        out.push(if is_space { ' ' } else { c });
    }
    while out.ends_with(char::is_whitespace) {
        out.pop();
    }
    out
}

/// Every commit reachable from a ref's current tip, which is what decides whether an
/// entry counts as unreachable for `--expire-unreachable`.
/// `reflog_expiry_prepare()`'s three reachability regimes.
#[derive(Clone, Copy, PartialEq)]
enum Unreachable {
    /// `UE_ALWAYS`: nothing is consulted; age alone decides.
    Always,
    /// `UE_HEAD`: every ref is a tip.
    Head,
    /// `UE_NORMAL`: the ref's own tip is the only one.
    Normal,
}

/// `is_head()` (reflog.c:439-444): the ref name with any worktree prefix stripped
/// is exactly `HEAD`.
fn is_head_log(full_name: &str) -> bool {
    full_name == "HEAD"
        || full_name
            .rsplit_once('/')
            .is_some_and(|(head, tail)| tail == "HEAD" && head.starts_with("worktrees/"))
}

/// `lookup_commit_reference_gently(the_repository, oid, 1)` on a ref's target: the
/// commit it names, or `None` when it names none.
fn ref_tip_commit(repo: &gix::Repository, full_name: &str) -> Option<ObjectId> {
    repo.try_find_reference(full_name)
        .ok()
        .flatten()
        .and_then(|mut r| r.peel_to_id_in_place().ok())
        .filter(|id| repo.find_commit(id.detach()).is_ok())
        .map(|id| id.detach())
}

/// `push_tip_to_list()` over `refs_for_each_ref()`, closed over: the commits every
/// non-symbolic ref reaches, which is `UE_HEAD`'s notion of reachable.
fn reachable_from_all_refs(
    repo: &gix::Repository,
) -> Result<std::collections::HashSet<ObjectId>> {
    let mut tips: Vec<ObjectId> = Vec::new();
    if let Ok(platform) = repo.references() {
        if let Ok(iter) = platform.all() {
            for reference in iter.flatten() {
                let mut reference = reference;
                if reference.target().try_id().is_none() {
                    continue; // `if (ref->flags & REF_ISSYMREF) return 0;`
                }
                if let Ok(id) = reference.peel_to_id_in_place() {
                    if repo.find_commit(id.detach()).is_ok() {
                        tips.push(id.detach());
                    }
                }
            }
        }
    }
    let mut set = std::collections::HashSet::new();
    if let Ok(walk) = repo.rev_walk(tips).all() {
        for info in walk.flatten() {
            set.insert(info.id);
        }
    }
    Ok(set)
}

fn reachable_from_ref(
    repo: &gix::Repository,
    full_name: &str,
) -> Result<std::collections::HashSet<ObjectId>> {
    let mut set = std::collections::HashSet::new();
    let Some(tip) = repo
        .try_find_reference(full_name)
        .ok()
        .flatten()
        .and_then(|mut r| r.peel_to_id_in_place().ok())
        .map(|id| id.detach())
    else {
        return Ok(set);
    };
    let Ok(walk) = repo.rev_walk([tip]).all() else {
        return Ok(set);
    };
    for info in walk.flatten() {
        set.insert(info.id);
    }
    Ok(set)
}

#[cfg(test)]
mod drop_write_tests {
    use super::{is_root_ref, normalize_reflog_message};

    /// `copy_reflog_msg()`: a run of whitespace becomes one space, a leading run is
    /// dropped, and the result is right-trimmed — so the entry stays one line however
    /// the message was typed. Verified against stock git 2.55.0, where
    /// `git reflog write <ref> <old> <new> "  lots   of\n\nwhitespace\there   "`
    /// records `lots of whitespace here`.
    #[test]
    fn a_message_is_collapsed_to_single_spaces_and_trimmed() {
        assert_eq!(
            normalize_reflog_message("  lots   of\n\nwhitespace\there   "),
            "lots of whitespace here"
        );
        assert_eq!(normalize_reflog_message("plain"), "plain");
    }

    /// An empty result means no tab and no message at all in the written line, which
    /// is `log_ref_write_fd()`'s `if (msg && *msg)`. Stock writes the same line for
    /// `""` and for `"   "`.
    #[test]
    fn a_message_of_only_whitespace_becomes_nothing() {
        assert_eq!(normalize_reflog_message(""), "");
        assert_eq!(normalize_reflog_message("   "), "");
        assert_eq!(normalize_reflog_message("\n\t "), "");
    }

    /// `is_root_ref()`: upper-case-`-`-`_` syntax, not one of the two pseudo-refs, and
    /// then either `_HEAD`-suffixed or on the irregular list. This is what lets
    /// `reflog write` name a root ref at all — everything else has to pass
    /// `check_refname_format(ref, 0)`, which one-level names fail. Each of these was
    /// run against stock git 2.55.0: `HEAD`, `ORIG_HEAD`, `AUTO_MERGE` and `FOO_HEAD`
    /// are written, while `MERGE_HEAD`, `onelevel` and `FOO` are
    /// `fatal: invalid reference name`.
    #[test]
    fn only_git_s_root_refs_skip_the_refname_check() {
        for ok in ["HEAD", "ORIG_HEAD", "FOO_HEAD", "AUTO_MERGE", "MERGE_AUTOSTASH"] {
            assert!(is_root_ref(ok), "{ok} is a root ref");
        }
        // The two pseudo-refs are excluded even though they match the syntax.
        for no in ["MERGE_HEAD", "FETCH_HEAD", "FOO", "onelevel", "refs/heads/main", ""] {
            assert!(!is_root_ref(no), "{no} is not a root ref");
        }
    }
}
