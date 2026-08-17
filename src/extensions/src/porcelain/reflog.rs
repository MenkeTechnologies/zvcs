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
///     [--updateref] [--dry-run] [--all | <ref>…]` — `should_expire_reflog_ent()`'s
///     two tests: an entry goes when it is older than the total cutoff, and also when
///     it is unreachable from the ref and older than the unreachable cutoff. Without
///     the options git's 90-day / 30-day defaults apply. An emptied log is left as an
///     empty file, as git leaves it.
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
    /// `core.quotePath`, which decides whether bytes >= 0x80 are octal-escaped.
    quote_high: bool,
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
            quote_high: true,
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
    let mut opts = Opts {
        quote_high: repo
            .config_snapshot()
            .boolean("core.quotePath")
            .unwrap_or(true),
        ..Opts::default()
    };

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
                match classify_pretty(v) {
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
                saw_ref_source = true;
                match resolve_spec(repo, s)? {
                    Resolved::Section(section) => sections.push(section),
                    // Resolves to an object but owns no reflog: git prints nothing.
                    Resolved::Empty => {}
                    Resolved::Fatal(code) => return Ok(code),
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
                opts.quote_high,
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

    let (base, selector) = split_selector(spec);

    let entries = read_entries(repo, base)?;

    match selector {
        None => {
            let Some(entries) = entries else {
                // Not a ref with a log, so `setup_revisions()` decides the
                // operand's fate the way it decides every other one:
                //
                // ```c
                // if (get_oid_with_context(revs->repo, arg, get_sha1_flags, &oid, &oc)) {
                //         ret = revs->ignore_missing ? 0 : -1;
                //         goto out;
                // }
                // if (!cant_be_filename)
                //         verify_non_filename(the_repository, revs->prefix, arg);
                // object = get_reference(revs, arg, &oid, flags ^ local_flags);
                // ```
                //
                // `get_oid_with_context()` is the full-hex rule — an id the
                // repository does not have still resolves, without the object
                // database being consulted — and `get_reference()` then
                // `parse_object()`s it and `die(_("bad object %s"), name)`s.
                // Only a name that resolves to nothing at all reaches
                // `setup_revisions()`'s `ambiguous argument` block, which is why
                // `git reflog show <absent-40-hex>` is `bad object` in stock
                // 2.55.0 and `git reflog show nosuchref` is not.
                //
                // Quiet, because this function already reached
                // `warn_ambiguous_refname` for `spec` above and git resolves the
                // operand once.
                return Ok(if crate::objname::resolves_but_absent(repo, base) {
                    eprintln!("fatal: bad object {base}");
                    Resolved::Fatal(128)
                } else if crate::objname::resolve_quiet(repo, base).is_some() {
                    Resolved::Empty
                } else {
                    Resolved::Fatal(fatal_ambiguous(spec))
                });
            };
            Ok(Resolved::Section(Section {
                display: base.to_owned(),
                full: full_name(repo, base),
                start: 0,
                selector: SelectorKind::None,
                entries,
            }))
        }
        Some(Selector::Index(n)) => {
            let Some(entries) = entries else {
                return Ok(Resolved::Fatal(fatal_ambiguous(spec)));
            };
            if entries.is_empty() {
                return Ok(Resolved::Fatal(fatal_ambiguous(spec)));
            }
            if n >= entries.len() {
                eprintln!("fatal: log for '{base}' only has {} entries", entries.len());
                return Ok(Resolved::Fatal(128));
            }
            Ok(Resolved::Section(Section {
                display: base.to_owned(),
                full: full_name(repo, base),
                start: n,
                selector: SelectorKind::Index,
                entries,
            }))
        }
        Some(Selector::Date(text)) => {
            let Some(entries) = entries else {
                return Ok(Resolved::Fatal(fatal_ambiguous(spec)));
            };
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
                    eprintln!(
                        "warning: log for '{base}' only goes back to {}",
                        oldest.time.format_or_unix(tfmt::RFC2822)
                    );
                }
            }
            Ok(Resolved::Section(Section {
                display: base.to_owned(),
                full: full_name(repo, base),
                start,
                // A date selector switches only this section's column to date form.
                selector: SelectorKind::Date,
                entries,
            }))
        }
    }
}

/// Read a ref's whole reflog, flipped into git's newest-first order.
/// `None` means the ref has no log file (or does not exist).
fn read_entries(repo: &gix::Repository, name: &str) -> Result<Option<Vec<Entry>>> {
    let Some(reference) = repo.try_find_reference(name).ok().flatten() else {
        return Ok(None);
    };
    let mut platform = reference.log_iter();
    let Some(iter) = platform.all()? else {
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

/// The full ref name behind a ref as typed, for `%gD`. Falls back to the input.
fn full_name(repo: &gix::Repository, name: &str) -> String {
    match repo.try_find_reference(name).ok().flatten() {
        Some(r) => r.name().as_bstr().to_str_lossy().into_owned(),
        None => name.to_owned(),
    }
}

/// git's `shorten_unambiguous_ref` for the reflog selector display (`%gd`): a full
/// ref name is shown by its canonical short form regardless of how it was typed —
/// `refs/heads/main` → `main`, `refs/stash` → `stash`, `HEAD` stays `HEAD`.
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
}

/// Classify a `--pretty=`/`--format=` value the way git's `get_commit_format()` does.
fn classify_pretty(value: &str) -> Pretty {
    if let Some(rest) = value
        .strip_prefix("format:")
        .or_else(|| value.strip_prefix("tformat:"))
    {
        return Pretty::Custom(rest.to_owned());
    }
    match value {
        "oneline" => Pretty::Oneline,
        "medium" => Pretty::Builtin(Builtin::Medium),
        "short" => Pretty::Builtin(Builtin::Short),
        "full" => Pretty::Builtin(Builtin::Full),
        "fuller" => Pretty::Builtin(Builtin::Fuller),
        "raw" => Pretty::Builtin(Builtin::Raw),
        "reference" => Pretty::Builtin(Builtin::Reference),
        // The mbox/patch formats need git's whole email driver; still deferred.
        "email" | "mboxrd" => Pretty::Unimplemented,
        v if v.is_empty() || v.contains('%') => Pretty::Custom(v.to_owned()),
        _ => Pretty::Invalid,
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
                // `%gd` is the short selector (git's `shorten_unambiguous_ref`:
                // `refs/stash` → `stash`), `%gD` the full ref. Both are independent
                // of the oneline path, which prints the ref as it was typed.
                let name = if kind == b'd' {
                    shorten_ref(&section.full)
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
    quote_high: bool,
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
                    out.extend_from_slice(&quote_path(source, quote_high));
                    out.push(b'\t');
                    out.extend_from_slice(&quote_path(&change.path, quote_high));
                }
                None => {
                    out.push(b'\t');
                    out.extend_from_slice(&quote_path(&change.path, quote_high));
                }
            }
            out.push(b'\n');
        }
    }

    if fmts.name_only {
        for change in changes {
            out.extend_from_slice(&quote_path(&change.path, quote_high));
            out.push(b'\n');
        }
    }

    if fmts.name_status {
        for change in changes {
            match &change.source {
                Some(source) => {
                    out.push(change.kind.letter());
                    out.extend_from_slice(format!("{:03}\t", change.score).as_bytes());
                    out.extend_from_slice(&quote_path(source, quote_high));
                    out.push(b'\t');
                }
                None => {
                    out.push(change.kind.letter());
                    out.push(b'\t');
                }
            }
            out.extend_from_slice(&quote_path(&change.path, quote_high));
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
            out.extend_from_slice(&display_name(change, quote_high));
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
                    append_mode_name(out, "create", change.new_mode, &change.path, quote_high);
                }
                ChangeKind::Deleted => {
                    append_mode_name(out, "delete", change.old_mode, &change.path, quote_high);
                }
                ChangeKind::Renamed | ChangeKind::Copied => {
                    let verb = if change.kind == ChangeKind::Renamed {
                        "rename"
                    } else {
                        "copy"
                    };
                    out.extend_from_slice(format!(" {verb} ").as_bytes());
                    out.extend_from_slice(&display_name(change, quote_high));
                    out.extend_from_slice(format!(" ({}%)\n", change.score).as_bytes());
                    // git names the file only on a standalone mode change.
                    append_mode_change(out, change, None, quote_high);
                }
                ChangeKind::Modified => {
                    append_mode_change(out, change, Some(&change.path), quote_high);
                }
            }
        }
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// ` create mode 100644 <path>`, git's `show_file_mode_name()`.
fn append_mode_name(out: &mut Vec<u8>, verb: &str, mode: Option<u16>, path: &[u8], high: bool) {
    match mode {
        Some(mode) => out.extend_from_slice(format!(" {verb} mode {mode:06o} ").as_bytes()),
        None => out.extend_from_slice(format!(" {verb} ").as_bytes()),
    }
    out.extend_from_slice(&quote_path(path, high));
    out.push(b'\n');
}

/// git's `show_mode_change()`: only when both sides have a mode and they differ.
fn append_mode_change(out: &mut Vec<u8>, change: &FileChange, name: Option<&[u8]>, high: bool) {
    let (Some(old), Some(new)) = (change.old_mode, change.new_mode) else {
        return;
    };
    if old == new {
        return;
    }
    out.extend_from_slice(format!(" mode change {old:06o} => {new:06o}").as_bytes());
    if let Some(name) = name {
        out.push(b' ');
        out.extend_from_slice(&quote_path(name, high));
    }
    out.push(b'\n');
}

/// The name a change is shown under: the compacted `a => b` form for a rename or
/// copy, the quoted path otherwise.
fn display_name(change: &FileChange, quote_high: bool) -> Vec<u8> {
    match &change.source {
        Some(source) => pprint_rename(source, &change.path, quote_high),
        None => quote_path(&change.path, quote_high),
    }
}

/// git's `pprint_rename()`. When neither side needs quoting it factors out the
/// common directory prefix and the common suffix into `pfx{old => new}sfx`;
/// otherwise it falls back to two separately quoted names.
fn pprint_rename(a: &[u8], b: &[u8], quote_high: bool) -> Vec<u8> {
    if needs_quoting(a, quote_high) || needs_quoting(b, quote_high) {
        let mut out = quote_path(a, quote_high);
        out.extend_from_slice(b" => ");
        out.extend_from_slice(&quote_path(b, quote_high));
        return out;
    }

    // The common prefix only counts up to the last slash inside it.
    let mut prefix = 0usize;
    let mut i = 0usize;
    while i < a.len() && i < b.len() && a[i] == b[i] {
        if a[i] == b'/' {
            prefix = i + 1;
        }
        i += 1;
    }

    // Walk backwards from the terminator. When a prefix was found it ends in a
    // slash, and git lets this loop run one byte into it to see that same slash.
    let mut suffix = 0usize;
    let floor = prefix.saturating_sub(usize::from(prefix > 0));
    let (mut ai, mut bi) = (a.len(), b.len());
    loop {
        if ai < floor || bi < floor {
            break;
        }
        // Index `len` stands for the NUL terminator git compares first.
        let ca = a.get(ai).copied().unwrap_or(0);
        let cb = b.get(bi).copied().unwrap_or(0);
        if ca != cb {
            break;
        }
        if ca == b'/' {
            suffix = a.len() - ai;
        }
        if ai == 0 || bi == 0 {
            break;
        }
        ai -= 1;
        bi -= 1;
    }

    let a_mid = a.len().saturating_sub(prefix + suffix);
    let b_mid = b.len().saturating_sub(prefix + suffix);
    let mut out = Vec::with_capacity(prefix + a_mid + b_mid + suffix + 7);
    let braced = prefix + suffix > 0;
    if braced {
        out.extend_from_slice(&a[..prefix]);
        out.push(b'{');
    }
    out.extend_from_slice(&a[prefix..prefix + a_mid]);
    out.extend_from_slice(b" => ");
    out.extend_from_slice(&b[prefix..prefix + b_mid]);
    if braced {
        out.push(b'}');
        out.extend_from_slice(&a[a.len() - suffix..]);
    }
    out
}

// ---------------------------------------------------------------------------
// path quoting
// ---------------------------------------------------------------------------

/// How git's `cq_lookup` table classifies one byte.
enum Quoted {
    /// Emitted as-is.
    Literal,
    /// Emitted as a backslash followed by this byte.
    Escaped(u8),
    /// Emitted as a three-digit octal escape.
    Octal,
}

/// `quote_high` is `core.quotePath`, which decides whether bytes >= 0x80 are
/// octal-escaped or passed through.
fn classify_byte(byte: u8, quote_high: bool) -> Quoted {
    match byte {
        0x07 => Quoted::Escaped(b'a'),
        0x08 => Quoted::Escaped(b'b'),
        0x09 => Quoted::Escaped(b't'),
        0x0a => Quoted::Escaped(b'n'),
        0x0b => Quoted::Escaped(b'v'),
        0x0c => Quoted::Escaped(b'f'),
        0x0d => Quoted::Escaped(b'r'),
        b'"' => Quoted::Escaped(b'"'),
        b'\\' => Quoted::Escaped(b'\\'),
        b if b < 0x20 || b == 0x7f => Quoted::Octal,
        b if b >= 0x80 && quote_high => Quoted::Octal,
        _ => Quoted::Literal,
    }
}

fn needs_quoting(path: &[u8], quote_high: bool) -> bool {
    path.iter()
        .any(|&b| !matches!(classify_byte(b, quote_high), Quoted::Literal))
}

/// git's `quote_c_style()`: a path that needs no escape is printed bare, and one
/// that needs any is wrapped in double quotes with C-style escapes throughout.
fn quote_path(path: &[u8], quote_high: bool) -> Vec<u8> {
    if !needs_quoting(path, quote_high) {
        return path.to_vec();
    }
    let mut out = Vec::with_capacity(path.len() + 2);
    out.push(b'"');
    for &byte in path {
        match classify_byte(byte, quote_high) {
            Quoted::Literal => out.push(byte),
            Quoted::Escaped(c) => {
                out.push(b'\\');
                out.push(c);
            }
            Quoted::Octal => out.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
        }
    }
    out.push(b'"');
    out
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
    let Some(open) = spec.rfind("@{") else {
        return (spec, None);
    };
    if !spec.ends_with('}') || open == 0 {
        return (spec, None);
    }
    let inner = &spec[open + 2..spec.len() - 1];
    match inner.parse::<usize>() {
        Ok(n) => (&spec[..open], Some(Selector::Index(n))),
        Err(_) => (&spec[..open], Some(Selector::Date(inner))),
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

/// The file a ref's reflog lives in. `HEAD` (and the other per-worktree
/// pseudo-refs) belong to this worktree; everything else is shared.
pub(crate) fn log_file(repo: &gix::Repository, full_name: &str) -> PathBuf {
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
            "--verbose" => {}
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

    for spec in selectors {
        let Some((name, rest)) = spec.split_once("@{") else {
            eprintln!("error: not a reflog: {spec}");
            return Ok(ExitCode::from(255));
        };
        let Some(index) = rest.strip_suffix('}').and_then(|n| n.parse::<usize>().ok()) else {
            eprintln!("error: not a reflog: {spec}");
            return Ok(ExitCode::from(255));
        };
        let full = resolve_log_ref(repo, name);
        let path = log_file(repo, &full);
        let Some(mut lines) = read_raw_log(&path)? else {
            eprintln!("error: reflog for '{name}' does not exist");
            return Ok(ExitCode::from(255));
        };
        // `@{0}` is the newest entry, which is the last line of the file.
        let Some(at) = lines.len().checked_sub(index + 1) else {
            continue;
        };
        lines.remove(at);
        if dry_run {
            continue;
        }
        write_raw_log(&path, &lines, rewrite)?;
        if updateref {
            if let Some(newest) = lines.last() {
                update_ref_to(repo, &full, newest.new)?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Point `full_name` at `oid` without adding a reflog entry of its own, which is what
/// `--updateref` does after the log was rewritten.
fn update_ref_to(repo: &gix::Repository, full_name: &str, oid: ObjectId) -> Result<()> {
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::Only,
                force_create_reflog: false,
                message: "".into(),
            },
            expected: PreviousValue::Any,
            new: gix::refs::Target::Object(oid),
        },
        name: full_name
            .try_into()
            .map_err(|e| anyhow!("invalid ref name {full_name}: {e}"))?,
        deref: false,
    })?;
    Ok(())
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
    let mut dry_run = false;
    let mut rewrite = false;
    let mut updateref = false;
    let mut expire: Option<i64> = None;
    let mut expire_unreachable: Option<i64> = None;
    let mut refs: Vec<String> = Vec::new();
    // `now` is "everything is older", `never` is "nothing is"; anything else is an
    // approxidate the same parser `--expire` takes elsewhere reads.
    let cutoff = |value: &str| -> Option<i64> {
        match value {
            "now" | "all" => Some(i64::MAX),
            "never" | "false" => Some(i64::MIN),
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
            "--single-worktree" => {}
            "-n" | "--dry-run" => dry_run = true,
            "--rewrite" => rewrite = true,
            "--updateref" => updateref = true,
            "--stale-fix" | "--verbose" => {}
            _ if s.starts_with("--expire-unreachable=") => {
                let v = &s["--expire-unreachable=".len()..];
                match cutoff(v) {
                    Some(t) => expire_unreachable = Some(t),
                    None => crate::git_fatal!("'{v}' is not a valid timestamp"),
                }
            }
            _ if s.starts_with("--expire=") => {
                let v = &s["--expire=".len()..];
                match cutoff(v) {
                    Some(t) => expire = Some(t),
                    None => crate::git_fatal!("'{v}' is not a valid timestamp"),
                }
            }
            _ if s.starts_with('-') && s != "-" => {
                return Ok(super::unknown_option(s, EXPIRE_USAGE))
            }
            _ => refs.push(s.to_owned()),
        }
    }

    let expire = expire.unwrap_or(now - 90 * DAY);
    let expire_unreachable = expire_unreachable.unwrap_or(now - 30 * DAY);

    let targets: Vec<String> = if all {
        let mut names = Vec::new();
        for root in reflog_roots(repo) {
            collect_logs(&root, "", &mut names)?;
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
        let path = log_file(repo, &full);
        let Some(lines) = read_raw_log(&path)? else {
            continue;
        };
        // `should_expire_reflog_ent()`: an entry goes when it is older than the total
        // cutoff, and separately when it is unreachable and older than that cutoff. The
        // reachable set is only needed for the second test.
        let reachable = (expire_unreachable != i64::MIN)
            .then(|| reachable_from_ref(repo, &full))
            .transpose()?;
        let mut kept: Vec<RawLine> = Vec::new();
        for line in lines {
            if line.time < expire {
                continue;
            }
            let unreachable = reachable
                .as_ref()
                .is_some_and(|set| !line.new.is_null() && !set.contains(&line.new));
            if unreachable && line.time < expire_unreachable {
                continue;
            }
            kept.push(line);
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

/// `refs_resolve_ref_unsafe(…, RESOLVE_REF_READING, …)`: the name `path` finally
/// resolves to after following symrefs, or `None` when `path` names no reference or
/// the chain does not end at an object.
///
/// gitoxide's `try_find` takes a *partial* name and will fall back to `refs/tags/`,
/// `refs/heads/` and `refs/remotes/` on its own. This caller is already walking git's
/// rule list itself, so a lookup that answered under a different name is discarded —
/// otherwise every candidate would resolve and the first one would always win.
pub(crate) fn resolve_ref_reading(repo: &gix::Repository, path: &str) -> Option<String> {
    // git's `SYMREF_MAXDEPTH`.
    const MAX_DEPTH: usize = 5;
    let mut name = path.to_owned();
    for _ in 0..MAX_DEPTH {
        let found = repo.refs.try_find(name.as_str()).ok().flatten()?;
        if found.name.as_bstr() != name.as_str() {
            return None;
        }
        match found.target {
            gix::refs::Target::Object(_) => return Some(name),
            gix::refs::Target::Symbolic(next) => name = next.as_bstr().to_str_lossy().into_owned(),
        }
    }
    None
}

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
            git_path_display(repo, &path)
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

/// `git_path()`'s rendering of a path inside the git directory. `setup.c` has already
/// moved to the top of the work tree by the time a message is printed, so an ordinary
/// repository shows its git directory as `.git` however deep the command was run.
fn git_path_display(repo: &gix::Repository, path: &Path) -> String {
    let git_dir = repo.git_dir();
    let shown = match repo.workdir() {
        Some(top) if git_dir == top.join(".git") => Path::new(".git"),
        _ => git_dir,
    };
    match path.strip_prefix(git_dir) {
        Ok(rest) => shown.join(rest).display().to_string(),
        Err(_) => path.display().to_string(),
    }
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
