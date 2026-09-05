use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::prelude::ObjectIdExt;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

use super::log::{approxidate, get_commit_format, rev_list_pretty_body, wildmatch, Pretty};
use crate::revfilter::{compile_patterns, CommitFilter, Dialect};

/// The usage block stock git prints on a usage error, verbatim. git exits 129
/// for these, not 1, so the block travels with an explicit exit code rather
/// than through `anyhow`.
const USAGE: &str = r"usage: git rev-list [<options>] <commit>... [--] [<path>...]

  limiting output:
    --max-count=<n>
    --max-age=<epoch>
    --min-age=<epoch>
    --sparse
    --no-merges
    --min-parents=<n>
    --no-min-parents
    --max-parents=<n>
    --no-max-parents
    --remove-empty
    --all
    --branches
    --tags
    --remotes
    --stdin
    --exclude-hidden=[fetch|receive|uploadpack]
    --quiet
  ordering output:
    --topo-order
    --date-order
    --reverse
  formatting output:
    --parents
    --children
    --objects | --objects-edge
    --disk-usage[=human]
    --unpacked
    --header | --pretty
    --[no-]object-names
    --abbrev=<n> | --no-abbrev
    --abbrev-commit
    --left-right
    --count
    -z
  special purpose:
    --bisect
    --bisect-vars
    --bisect-all
";

/// Print the usage block and return git's usage exit code.
fn usage_error() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// Print a `fatal:` line and return git's fatal exit code.
fn fatal(message: &str) -> ExitCode {
    eprintln!("fatal: {message}");
    ExitCode::from(128)
}

/// Print a diagnostic that already carries its own prefixes and newline, and
/// return git's fatal exit code.
///
/// [`fatal`]'s counterpart for a message that may be more than one line: a
/// symmetric range whose endpoint is not a commit makes `setup_revisions()` write
/// `lookup_commit_reference()`'s `error:` note *ahead* of the `fatal:`, so the two
/// travel as one string and neither line may be re-prefixed.
fn fatal_text(text: &str) -> ExitCode {
    eprint!("{text}");
    ExitCode::from(128)
}

/// Everything `setup_revisions()` writes for a revision argument it could not
/// resolve, shared with `git log` — `rev-list` runs the same `setup_revisions()`.
///
/// Prefixes and trailing newline included, which is why it pairs with
/// [`fatal_text`] rather than [`fatal`]: this is the only producer of the seed
/// walk's `Err(String)`, so that whole channel carries finished stderr text.
fn unresolvable(repo: &gix::Repository, spec: &str) -> String {
    super::log::bad_revision_message_in(repo, spec)
}

/// [`unresolvable`] with `handle_revision_arg()`'s `REVARG_CANNOT_BE_FILENAME` in
/// hand.
///
/// ```c
/// if (seen_dashdash)
///         revarg_opt |= REVARG_CANNOT_BE_FILENAME;
/// …
/// if (handle_revision_arg(arg, revs, flags, revarg_opt)) {
///         if (seen_dashdash || *arg == '^')
///                 die(_("bad revision '%s'"), arg);
/// ```
///
/// (`revision.c:3035-3036`, `revision.c:3080-3087`.) An argument vector that
/// carries a `--` anywhere, and every line `read_revisions_from_stdin()` reads,
/// take the short `bad revision '<arg>'` instead of `verify_filename()`'s
/// "ambiguous argument" block — stock 2.55.0 answers
/// `fatal: bad revision 'nosuchrev'` for `git rev-list nosuchrev -- base.txt`.
fn unresolvable_in(repo: &gix::Repository, spec: &str, cant_be_filename: bool) -> String {
    super::log::bad_revision_message_in_gated(repo, spec, cant_be_filename)
}

/// Reject a malformed integer flag value exactly as git does: `fatal: '<v>': not
/// an integer`, exit 128 — not the 129 usage path.
fn not_an_integer(value: &str) -> ExitCode {
    fatal(&format!("'{value}': not an integer"))
}

/// Parse a flag value the way git's `git_parse_signed` does: optional surrounding
/// ASCII whitespace, an optional sign, then base-10 digits with nothing trailing.
/// `0x10`, `3abc`, an empty string, and out-of-range values all fail — matching
/// git, which then dies "not an integer".
fn parse_git_int(value: &str) -> Option<i64> {
    value
        .trim_matches(|c: char| c.is_ascii_whitespace())
        .parse::<i64>()
        .ok()
}

/// Map a signed `--max-count`/`-n` to the internal limit: git treats any negative
/// value as "no limit" (its stored max_count stays -1), so those become `None`.
fn clamp_count(n: i64) -> Option<usize> {
    if n < 0 {
        None
    } else {
        Some(n as usize)
    }
}

/// How commits are ordered before filtering and limiting.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    /// Commit date, newest first — git's default.
    Date,
    /// `--topo-order`: no parent before all its children, branches kept contiguous.
    Topo,
    /// `--date-order`: no parent before all its children, otherwise by date.
    DateTopo,
    /// `--author-date-order`: the same walk, breaking ties by *author* date
    /// (`REV_SORT_BY_AUTHOR_DATE`, revision.c:2456-2458). The commit date a walk
    /// normally orders by is the one a rebase or an amend rewrites; the author
    /// date survives both.
    AuthorDateTopo,
}

/// Where one argument in the scanned vector came from.
///
/// `read_revisions_from_stdin()` (`revision.c:2937-2983`) reads its lines from
/// inside `setup_revisions()`'s own loop, so they are spliced into the vector at
/// the `--stdin` position — but they are not argv, and the reader treats them
/// differently in four ways: only pseudo-options are accepted, a failed revision
/// is `bad revision '<line>'` rather than a pathspec, `--end-of-options` switches
/// the option test off for the rest of the block, and the reader keeps its **own**
/// `int flags = 0` — so an argv `--not` written before `--stdin` does not reach
/// the stdin lines, and a `--not` among them does not reach the argv that follows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Argv,
    /// A `--stdin` line read before any `--end-of-options`.
    Stdin,
    /// A `--stdin` line read after one, which is a revision whatever it looks like.
    StdinAfterEndOfOptions,
    /// The sentinel closing a spliced block: restores the `--not` state the argv
    /// scan held when `--stdin` was reached.
    StdinEnd(bool),
}

/// Whether `handle_revision_pseudo_opt()` (`revision.c:2778-2935`) claims `arg`.
///
/// The list is that function's own `strcmp`/`skip_prefix`/`parse_long_opt` chain.
/// It decides one thing here: a `--stdin` line starting with `-` is either one of
/// these or `fatal: invalid option '<line>' in --stdin mode`. The detached forms
/// (`--glob <pat>`) cannot occur, because the reader hands the option a one-element
/// `argv` with no following element to take a value from.
fn is_revision_pseudo_opt(arg: &str) -> bool {
    const EXACT: &[&str] = &[
        "--all",
        "--branches",
        "--bisect",
        "--tags",
        "--remotes",
        "--reflog",
        "--indexed-objects",
        "--alternate-refs",
        "--not",
        "--no-walk",
        "--do-walk",
        "--single-worktree",
        "--no-filter",
    ];
    const PREFIX: &[&str] = &[
        "--glob=",
        "--exclude=",
        "--exclude-hidden=",
        "--branches=",
        "--tags=",
        "--remotes=",
        "--no-walk=",
        "--filter=",
    ];
    EXACT.contains(&arg) || PREFIX.iter().any(|p| arg.starts_with(p))
}

/// One command-line revision together with the flags `setup_revisions()` puts on
/// it. git keeps these as bits on the commit object; they are carried per seed
/// here and re-derived for the walked commits afterwards.
#[derive(Clone, Copy)]
struct Seed {
    id: ObjectId,
    /// `UNINTERESTING`: `^rev`, the left side of `a..b`, a merge base of `a...b`,
    /// or anything named while `--not` is in effect.
    uninteresting: bool,
    /// `SYMMETRIC_LEFT`: the left side of `a...b`, which `--left-right` marks `<`.
    symmetric_left: bool,
    /// `BOTTOM`: an explicitly excluded tip. `--ancestry-path` measures descent
    /// from these.
    bottom: bool,
}

/// One entry in git's `revs->pending` that never becomes a commit.
///
/// `prepare_revision_walk()` re-pends what `handle_commit()` declines to turn
/// into a commit — an annotated tag under its own name field, and a tree or blob
/// named on the command line or pulled out of the index — and
/// `traverse_non_commits()` (`list-objects.c:344-375`) walks that list *after*
/// every commit, in the order the arguments were read.
///
/// A tree or blob is pended only when `--objects` asked for object output
/// (`if (!revs->tree_objects) return NULL;`), which is why
/// `git rev-list main^{tree}` exits 0 having printed nothing at all.
#[derive(Clone)]
struct Pending {
    id: ObjectId,
    /// `pending->name` for a tag — the tag object's own name field — and
    /// `pending->path` for a tree or a blob, which is the path it was reached
    /// through and the base every entry under it is joined onto.
    name: Vec<u8>,
    kind: gix::object::Kind,
    /// `UNINTERESTING`. `handle_commit()` marks such a tree's contents and pends
    /// nothing; `traverse_non_commits()` then skips the object itself. Both
    /// happen before any traversal, so an excluded tree hides its contents
    /// whichever side of the interesting one it was written on.
    uninteresting: bool,
}

/// `--filter=<spec>`: which objects the `--objects` walk leaves out.
#[derive(Clone, Copy)]
enum Filter {
    /// `blob:none` — omit every blob.
    BlobNone,
    /// `blob:limit=<n>` — omit blobs of `n` bytes or more.
    BlobLimit(u64),
    /// `tree:<depth>` — omit every object whose depth from the root tree is at
    /// least `depth`. The root tree itself is depth 0, so `tree:0` omits
    /// everything and `tree:1` keeps only the root trees.
    TreeDepth(u64),
}

/// `--missing=<action>`: what an object the repository does not have costs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Missing {
    /// git's `MA_ERROR`, the default: a missing object is fatal.
    Error,
    /// `MA_ALLOW_ANY`: a missing object is skipped without a word.
    AllowAny,
    /// `MA_PRINT`: skipped, then listed as `?<oid>` in a section of its own.
    Print,
    /// `MA_PRINT_INFO`: the same section, with `path=` and `type=` appended to
    /// each line — "same as MA_PRINT but also prints missing object info"
    /// (builtin/rev-list.c:109).
    PrintInfo,
    /// `MA_ALLOW_PROMISOR`: skipped when the object is one a promisor pack
    /// promises, fatal otherwise — the shape a partial clone is expected to be
    /// in, where every absence is explained by the remote that still has it.
    AllowPromisor,
}

/// What the `--objects` walk lists, and what it does about objects the
/// repository does not have.
#[derive(Clone, Copy)]
struct ObjectWalk<'a> {
    filter: Option<Filter>,
    missing: Missing,
    /// `--filter-print-omitted`: whether an omit set is being collected, which
    /// is what makes the walk descend into a tree the filter excluded.
    collect_omits: bool,
    /// The `--` pathspecs, which restrict the listed trees and blobs the same
    /// way they restrict the commits. `None` means every object is listed.
    pathspecs: Option<&'a super::log::PathspecMatcher>,
}

/// `git rev-list` — list commit ids reachable from the given revisions.
///
/// Selecting what to walk:
/// ```text
///   * `rev-list <rev>...`            — commits reachable from each `<rev>`
///   * `rev-list ^<rev>`              — exclude commits reachable from `<rev>`
///   * `rev-list <a>..<b>`            — reachable from `<b>` but not `<a>` (empty
///                                       side defaults to `HEAD`)
///   * `rev-list <a>...<b>`           — the symmetric difference: reachable from
///                                       either side but not from their merge bases
///   * `--not`                        — flip the sense of the revisions that
///                                       follow, until the next `--not`
///   * `<rev>^@` / `<rev>^!`          — the parents alone, or the commit with its
///                                       parents excluded
///   * `<rev>^-[<n>]`                 — the commit with its `<n>`th parent
///                                       excluded (`<n>` defaults to 1), i.e.
///                                       `<rev> ^<rev>^<n>`
///   * `--all` / `--branches` / `--tags` / `--remotes`, each with an optional
///     `=<pattern>`, and `--glob=<pattern>` — seed from a ref set, at the position
///     the flag appears
///   * `--reflog`                     — `add_reflogs_to_pending()`: every id in
///                                       every log, which reaches commits no ref
///                                       points at any more
///   * `--exclude=<pattern>`          — drop refs from the next ref-set flag, and
///                                       only that one (`clear_ref_exclusions`)
///   * `--exclude-hidden=<section>`   — drop refs matching `transfer.hideRefs` and
///                                       `<section>.hideRefs` (`fetch`, `receive`
///                                       or `uploadpack`) from the ref-set flags
///                                       that see a full refname; the three
///                                       *narrowed* selectors are refused
///                                       alongside it because they do not
///   * `--indexed-objects`            — pend every index blob under its path and
///                                       every valid cache-tree, which contributes
///                                       objects but no commits
///   * `<tree-ish>` / `<blob>`        — `handle_commit()` pends these rather than
///                                       walking them, so they are not an error:
///                                       without `--objects` they list nothing at
///                                       exit 0, and with it they list themselves
///                                       and everything under them
///   * `--stdin`                      — read further revisions, and pathspecs
///                                       after a `--` line, from standard input,
///                                       *at the position the flag appears* and
///                                       with a `--not` state of its own; an
///                                       empty line ends the read, a line
///                                       starting with `-` must be a
///                                       pseudo-option, and a second `--stdin`
///                                       is `fatal: --stdin given twice?`
///   * `--no-walk[=(sorted|unsorted)]` — list the named commits only, in commit-date
///                                       order or in the order they were pended
/// ```
///
/// Shaping the walk:
/// ```text
///   * `--first-parent`               — follow only the first parent of merges
///   * `--topo-order` / `--date-order` — topological orderings
///   * `--ancestry-path`              — keep only descendants of the excluded tips
///   * `--merges` / `--no-merges` / `--{min,max}-parents=<n>` — parent-count filter
///   * `--since=` / `--until=` (`--after=` / `--before=`) — committer-date bounds
///                                       through `approxidate()`, and
///                                       `--max-age=` / `--min-age=` — the same
///                                       two bounds as a raw epoch
///   * `--sparse` / `--dense`         — whether a path-limited walk drops the
///                                       commits it found TREESAME (the in-place
///                                       parent prune happens either way)
///   * `--full-history`               — compare every parent instead of pruning a
///                                       merge onto the first parent it matches
///   * `--simplify-merges`            — `--full-history` plus the merge-collapsing
///                                       pass, which also forces topological order
///   * `--grep=` / `--author=` / `--committer=` with `-i`/`-E`/`-F`/`-P`,
///     `--all-match` and `--invert-grep` — header and message predicates
///   * `-- <path>...`                 — path-limited: keep only commits whose diff
///                                       against a parent touched a matching path
///   * `-n <n>` / `-n<n>` / `-<n>` / `--max-count=<n>` — limit the number listed
///   * `--reverse`                    — toggle reversed output (git XORs the flag,
///                                       so it applies with an odd count only;
///                                       limit applied first)
/// ```
///
/// Shaping the output:
/// ```text
///   * `--count`                      — print only counts, split by side under
///                                       `--left-right` / `--cherry-mark`
///   * `--quiet`                      — walk, print nothing
///   * `--parents` / `--children`     — append the commit's parents or children
///   * `--left-right` / `--cherry-mark` / `--boundary` — per-commit marks
///   * `--abbrev-commit` with `--abbrev[=<n>]` / `--no-abbrev` — shorten the object
///                                       name (git needs *both*: `--abbrev=<n>` on
///                                       its own leaves the id whole)
///   * `--header` / `--pretty[=<fmt>]` / `--format=<fmt>` — render each commit
///   * `--date=<mode>` / `--date <mode>` — the mode `%ad`/`%cd` and the header
///                                       lines render in (no `log.date` here:
///                                       that config belongs to `log`)
///   * `--[no-]commit-header`         — keep or drop the object-name line
///   * `--objects` / `--in-commit-order` / `--filter=<spec>` — also list the
///                                       trees and blobs reachable from the commits
///   * `--missing=(error|allow-any)`  — tolerate objects the repository lacks
///   * `--disk-usage[=human]`         — print the total on-disk size instead
///   * `--graph`                      — the ASCII ancestry graph in front of each
///                                       record, over the same renderer `log` uses;
///                                       it forces topological order and parent
///                                       rewriting, and takes over the `<`/`>`/`=`/`o`
///                                       marks. Not rendered — and so still refused —
///                                       beside `--objects`, `--count`, `--quiet` or
///                                       `--disk-usage`, which put lines between the
///                                       records or leave the records out
/// ```
///
/// `--[no-]encode-email-headers` is accepted and does nothing:
/// `builtin/rev-list.c` builds its `pretty_print_context` from scratch and never
/// copies `revs->encode_email_headers` into it, so — unlike `log` — the mail
/// formats here never see the switch (see [`super::log::EmailStyle::REV_LIST`]).
///
/// Genuinely unsupported forms stay rejected rather than silently accepted: `-z`,
/// the diff options `setup_revisions()` accepts and this command ignores (`-s` /
/// `--no-patch`, …), the `--missing` actions that need promisor plumbing,
/// `--bisect` under a pathspec (git weighs a TREESAME commit as reaching nothing
/// and there is no TREESAME marking during the walk here to weigh with), and a
/// `--cherry-mark` whose two sides are both non-empty, which is the only case
/// where git computes patch ids.
pub fn rev_list(args: &[String]) -> Result<ExitCode> {
    // `show_usage_if_asked(argc, argv, rev_list_usage)` (builtin/rev-list.c:711)
    // fires before the repository is opened, prints to stdout and exits 129 —
    // and only for a lone `-h`. Every other refusal is `usage()`, on stderr.
    if let Some(code) = super::show_usage_if_asked(args, USAGE) {
        return Ok(code);
    }

    let mut repo = match crate::setup::discover() {
        Ok(repo) => repo,
        Err(_) => {
            return Ok(fatal(
                "not a git repository (or any of the parent directories): .git",
            ))
        }
    };

    let mut count_only = false;
    let mut reverse = false;
    let mut first_parent = false;
    let mut objects = false;
    let mut in_commit_order = false;
    // `--[no-]object-names`: whether an object line carries the path it was
    // reached through. git prints the separator and the name only when it does.
    let mut object_names = true;
    let mut show_parents = false;
    let mut show_children = false;
    let mut boundary = false;
    let mut left_right = false;
    let mut cherry_mark = false;
    /// `revs->cherry_pick`: drop the commits whose change is already on the other side.
    let mut cherry_pick = false;
    /// `revs->left_only` / `revs->right_only`.
    let mut left_only = false;
    let mut right_only = false;
    let mut ancestry_path = false;
    // `revs->edge_hint`: print the uninteresting boundary as `-<id>` lines.
    let mut edge_hint = false;
    // `revs->exclude_promisor_objects`.
    let mut exclude_promisor = false;
    // `arg_print_omitted`.
    let mut print_omitted = false;
    // `revs->ancestry_path_bottoms` as `--ancestry-path=<commit>` fills it.
    let mut ancestry_bottoms: Vec<ObjectId> = Vec::new();
    let mut simplify_by_decoration = false;
    // `-g` / `--walk-reflogs`: `init_reflog_walk()` replaces the ancestry traversal
    // with a walk of each named ref's reflog, so the "revisions" are the entries
    // rather than the commits they point at.
    let mut walk_reflogs = false;
    // The revision words as typed, which is what the reflog walk needs: `main@{2}`
    // names both the log to read and the entry to start at.
    let mut reflog_names: Vec<String> = Vec::new();
    // `revs->show_pulls`: keep a merge that is TREESAME to a later parent but not
    // to its first — the merge that brought a change in rather than making it.
    let mut show_pulls = false;
    // `revs->exclude_first_parent_only`: the UNINTERESTING marking stops at each
    // excluded commit's first parent.
    let mut exclude_first_parent_only = false;
    // `revs->skip_count`: how many commits `get_revision()` throws away before it
    // starts answering, applied after every filter and ahead of `--max-count`.
    let mut skip_count: usize = 0;
    let mut bisect = false;
    // `--bisect-all`: `bisect_find_all` plus `BISECT_SHOW_ALL` and
    // `revs.show_decorations = 1` (builtin/rev-list.c) — every candidate is
    // listed, each decorated with the distance the search weighed it at.
    let mut bisect_all = false;
    // `--bisect-vars`: `bisect_show_vars`, which replaces the listing with the
    // six `bisect_*` shell assignments `git bisect` sources.
    let mut bisect_vars = false;
    let mut quiet = false;
    let mut disk_usage = false;
    let mut disk_usage_human = false;
    let mut include_header = true;
    let mut verbose_header = false;
    // `--timestamp`: prefix each object name with the commit date.
    let mut show_timestamp = false;
    // `--abbrev-commit`: shorten the object name rev-list prints, which is what
    // `--oneline` turns on together with the oneline format.
    let mut abbrev_commit = false;
    // `revs->abbrev`. `None` is git's `DEFAULT_ABBREV`, the auto-sized length
    // `builtin/rev-list.c` starts from; `Some(0)` is `--no-abbrev`, which turns
    // the abbreviation off however `--abbrev-commit` stands.
    let mut abbrev_len: Option<usize> = None;
    let mut pretty: Option<Pretty> = None;
    // `revs->date_mode`: the mode `%ad`/`%cd` and the header lines render in.
    // `rev-list` has no `log.date` equivalent — `git_log_config()` is `log`'s — so
    // the default stands until `--date=<mode>` moves it.
    let mut date_mode = super::log::DateMode::Default;
    let mut order = Order::Date;
    let mut filter: Option<Filter> = None;
    let mut missing = Missing::Error;
    // `--no-walk` and its `sorted`/`unsorted` argument (git's `unsorted_input`).
    let mut no_walk = false;
    let mut unsorted_input = false;
    let mut read_stdin = false;
    // git parses these as signed C ints. Negative min-parents lets every commit
    // through; negative max-count / max-parents mean "no limit" (git stores -1).
    let mut min_parents: i64 = 0;
    let mut max_parents: Option<usize> = None;
    let mut max_count: Option<usize> = None;
    // `--since`/`--until` are git's `max_age`/`min_age`, both committer-date.
    let mut max_age: Option<i64> = None;
    let mut min_age: Option<i64> = None;
    // `revs->max_age_as_filter` (`--since-as-filter`): the same bound as
    // `max_age`, applied as an output filter rather than as a walk cut.
    let mut max_age_as_filter: Option<i64> = None;
    // `revs->dense` (revision.c:2462-2465), which `repo_init_revisions()` starts
    // at 1. `--sparse` clears it and `--dense` puts it back.
    let mut dense = true;
    // `revs->simplify_history`, inverted. `--full-history` clears the flag so
    // `try_to_simplify_commit()` compares *every* parent and keeps the per-parent
    // verdicts instead of pruning the list to the first TREESAME parent.
    let mut full_history = false;
    // `revs->simplify_merges`, which also clears `simplify_history` and sets
    // `topo_order` and `rewrite_parents` (revision.c).
    let mut simplify_merges_opt = false;
    // `revs->remove_empty_trees` (revision.c). Listed in `rev-list`'s own usage
    // block since the beginning, and parsed by `handle_revision_opt()` for every
    // revision-walking command, but this file never read it — so a command stock
    // git answers was a 129 usage error here.
    let mut remove_empty = false;
    // `revs->graph`: `--graph` is a `revision.c` option, so `rev-list` draws the
    // same ASCII graph in front of its object names that `log` does.
    let mut graph = false;
    let mut pathspecs: Vec<Vec<u8>> = Vec::new();
    // `setup_revisions()`'s `seen_dashdash`, found in a scan of the whole
    // argument vector before anything is resolved.
    let seen_dashdash = args.iter().any(|a| a == "--");
    let mut seeds: Vec<Seed> = Vec::new();
    // `--exclude=<glob>`, held until the next ref-selecting option consumes it.
    let mut ref_excludes: Vec<String> = Vec::new();
    // `revs->ref_excludes.hidden_refs` and `.hidden_refs_configured`, installed by
    // `--exclude-hidden=<section>` and read by every later ref walk.
    let mut hidden_refs: Vec<String> = Vec::new();
    let mut hidden_configured = false;
    // Commits the command line already caused to be parsed. Only `--no-walk`
    // cares — see [`super::log::no_walk_uninteresting`].
    let mut parsed_commits: HashSet<ObjectId> = HashSet::new();
    // Annotated tag objects encountered while peeling seeds. `--objects` lists
    // them, named by the tag's own name field, ahead of any tree.
    let mut pending: Vec<Pending> = Vec::new();
    // git's `flags` in `setup_revisions`: `--not` XORs UNINTERESTING|BOTTOM onto
    // every revision named after it, and a leading `^` XORs it again.
    let mut negate = false;
    // git's `rev_input_given`: whether any revision argument or ref-set selector
    // was seen at all. A selector that matched no ref still counts, so
    // `--branches` in a repo with no branches lists nothing at exit 0.
    let mut rev_input_given = false;
    // The `--grep`/`--author`/`--committer` predicates and the dialect flags that
    // decide how their patterns compile.
    let mut grep_pats: Vec<String> = Vec::new();
    let mut author_pats: Vec<String> = Vec::new();
    let mut committer_pats: Vec<String> = Vec::new();
    let mut dialect = Dialect::Basic;
    let mut ignore_case = false;
    let mut all_match = false;
    let mut invert_grep = false;

    // git's argument vector, with each `--stdin` line spliced in *at the position
    // the option was written*: `setup_revisions()` calls
    // `read_revisions_from_stdin()` from inside its own argument loop
    // (`revision.c:3058`), so `printf dup | git rev-list --stdin --not tri` reads
    // `dup` while `flags` is still empty and only then meets `--not`. Reading
    // stdin after the loop instead inverts that — and produces a different commit
    // set at exit 0, not merely a different warning order.
    let mut argv: Vec<String> = args.to_vec();
    let mut origin: Vec<Origin> = vec![Origin::Argv; argv.len()];

    let mut i = 0;
    'args: while i < argv.len() {
        // Cloned rather than borrowed: a `--stdin` reached here splices its lines
        // into `argv` while the scan is still running.
        let a = argv[i].clone();
        let a = a.as_str();
        // A `--stdin` line that is not a revision is a *pseudo*-option or nothing:
        // `read_revisions_from_stdin()` never reaches `handle_revision_opt()`.
        //
        // ```c
        // if (!seen_end_of_options && sb.buf[0] == '-') {
        //         const char *argv[] = { sb.buf, NULL };
        //         if (!strcmp(sb.buf, "--end-of-options")) { seen_end_of_options = 1; continue; }
        //         if (handle_revision_pseudo_opt(revs, argv, &flags) > 0) continue;
        //         die(_("invalid option '%s' in --stdin mode"), sb.buf);
        // }
        // ```
        //
        // (`revision.c:2962-2973`.)
        if let Origin::StdinEnd(saved) = origin[i] {
            negate = saved;
            i += 1;
            continue 'args;
        }
        if origin[i] == Origin::Stdin && a.starts_with('-') && !is_revision_pseudo_opt(a) {
            return Ok(fatal(&format!("invalid option '{a}' in --stdin mode")));
        }
        // How many revisions were already pending when this argument was reached,
        // so the `--no-walk` rule at the end of the iteration can see what it added.
        let seeds_before = seeds.len();
        // `seen_end_of_options` inside the stdin reader: every line after it goes
        // straight to `handle_revision_arg()`, whatever it starts with.
        if origin[i] == Origin::StdinAfterEndOfOptions {
            super::log::warn_operand(&repo, a, false);
            if let Err(e) = seed_revision(&repo, a, negate, true, &mut seeds, &mut pending) {
                return Ok(fatal_text(&e));
            }
            note_parsed(&repo, a, &seeds[seeds_before..], &mut parsed_commits)?;
            rev_input_given = true;
            if seeds[seeds_before..].iter().any(|s| s.uninteresting) {
                no_walk = false;
            }
            i += 1;
            continue 'args;
        }
        // git's `parse_long_opt` takes a value attached (`--grep=x`) or detached
        // (`--grep x`); these are the rev-list options that carry one.
        for (name, sink) in [
            ("grep", &mut grep_pats as &mut Vec<String>),
            ("author", &mut author_pats),
            ("committer", &mut committer_pats),
        ] {
            if let Some(v) = long_opt_value(&argv, i, name) {
                sink.push(v.value);
                i += v.consumed;
                continue 'args;
            }
        }
        for name in ["since", "after"] {
            if let Some(v) = long_opt_value(&argv, i, name) {
                max_age = Some(approxidate(&v.value));
                i += v.consumed;
                continue 'args;
            }
        }
        for name in ["until", "before"] {
            if let Some(v) = long_opt_value(&argv, i, name) {
                min_age = Some(approxidate(&v.value));
                i += v.consumed;
                continue 'args;
            }
        }
        // ```c
        // } else if ((argcount = parse_long_opt("since-as-filter", argv, &optarg))) {
        //         revs->max_age_as_filter = approxidate(optarg);
        //         return argcount;
        // }
        // ```
        //
        // (revision.c:2282-2285.) `revs->max_age_as_filter` reads the same clock as
        // `--since` and takes the same `approxidate()` value, but it is applied
        // where `--until` is — `limit_list()` skips the commit and keeps walking
        // (revision.c:1446-1448) — instead of marking it UNINTERESTING. So an old
        // commit drops out of the output without taking its ancestors' *newer*
        // side of a merge with it, which is the whole point of the flag.
        if let Some(v) = long_opt_value(&argv, i, "since-as-filter") {
            max_age_as_filter = Some(approxidate(&v.value));
            i += v.consumed;
            continue 'args;
        }
        // `--max-age`/`--min-age` set the very same `revs->max_age`/`revs->min_age`
        // as `--since`/`--until` (revision.c:2379-2393); only the value parser
        // differs — [`super::log::parse_age`]'s raw epoch instead of
        // `approxidate()`, so a value that is not a number is fatal rather than
        // silently "now".
        for name in ["max-age", "min-age"] {
            let Some(v) = long_opt_value(&argv, i, name) else {
                continue;
            };
            // `parse_long_opt()` (diff.c:5380-5399) dies when the detached form
            // runs off the end of argv; [`long_opt_value`] answers with an empty
            // string there, which is a *different* value git accepts as written.
            if v.consumed == 2 && argv.get(i + 1).is_none() {
                return Ok(fatal(&format!("Option '--{name}' requires a value")));
            }
            let Ok(age) = super::log::parse_age(&v.value) else {
                return Ok(fatal(&format!(
                    "'{}': not a number of seconds since epoch",
                    v.value
                )));
            };
            if name == "max-age" {
                max_age = age;
            } else {
                min_age = age;
            }
            i += v.consumed;
            continue 'args;
        }
        match a {
            "--count" => count_only = true,
            // git toggles this with `revs->reverse ^= 1`, so an even number of
            // `--reverse` flags cancels out and leaves the default order.
            "--reverse" => reverse = !reverse,
            "--first-parent" => first_parent = true,
            "--objects" => objects = true,
            // ```c
            // } else if (!strcmp(arg, "--objects-edge")) {
            //         revs->tag_objects = 1;
            //         revs->tree_objects = 1;
            //         revs->blob_objects = 1;
            //         revs->edge_hint = 1;
            // ```
            //
            // (`revision.c`.) The object listing plus `mark_edges_uninteresting()`,
            // whose `show_edge` callback prints `-<id>` for every *uninteresting*
            // parent of a shown commit — which is how `pack-objects` learns the
            // boundary to delta against. The `-aggressive` spelling widens which
            // trees are marked, not which commits are printed.
            "--objects-edge" | "--objects-edge-aggressive" => {
                objects = true;
                edge_hint = true;
            }
            "--object-names" => object_names = true,
            "--no-object-names" => object_names = false,
            "--in-commit-order" => in_commit_order = true,
            "--parents" => show_parents = true,
            "--children" => show_children = true,
            "--boundary" => boundary = true,
            "--left-right" => left_right = true,
            "--cherry-mark" => cherry_mark = true,
            "--cherry-pick" => cherry_pick = true,
            "--left-only" => left_only = true,
            "--right-only" => right_only = true,
            // `OPT_SET_INT('\0', "cherry", …)`: `--cherry` is the shorthand
            // `--right-only --cherry-mark --no-merges` (revision.c).
            "--cherry" => {
                right_only = true;
                cherry_mark = true;
                max_parents = Some(1);
            }
            // ```c
            // if (revs->exclude_promisor_objects)
            //         odb_for_each_object(revs->repo->objects, NULL, mark_uninteresting,
            //                             revs, ODB_FOR_EACH_OBJECT_PROMISOR_ONLY);
            // ```
            //
            // (`prepare_revision_walk()`, revision.c:4001-4003.) Every object a
            // promisor pack holds is marked UNINTERESTING before the walk starts,
            // so the traversal never crosses from the objects this repository
            // realized into the ones the remote still owns. In a fresh partial
            // clone that is *everything*, and the listing is empty.
            // ```c
            // } else if (!strcmp(arg, "--exclude-promisor-objects")) {
            //         fetch_if_missing = 0;
            //         revs.exclude_promisor_objects = 1;
            // ```
            "--exclude-promisor-objects" => {
                gix::odb::store::set_fetch_if_missing(false);
                exclude_promisor = true;
            }
            // `--filter-print-omitted`: the objects the filter left out, as
            // `~<oid>` lines after the listing (builtin/rev-list.c:817-818,
            // 989-996).
            "--filter-print-omitted" => print_omitted = true,
            "--ancestry-path" => ancestry_path = true,
            // ```c
            // } else if (skip_prefix(arg, "--ancestry-path=", &optarg)) {
            //         revs->ancestry_path = 1;
            //         revs->simplify_history = 0;
            //         revs->limited = 1;
            //
            //         if (repo_get_oid_committish(revs->repo, optarg, &oid))
            //                 return error(msg, optarg);
            //         …
            //         commit_list_insert(c, &revs->ancestry_path_bottoms);
            // ```
            //
            // (`revision.c:2411-2426`.) The named commits *replace* the bottoms
            // the range would have supplied — `ancestry_path_implicit_bottoms`
            // stays 0 — and they accumulate, so several may be given.
            s if s.starts_with("--ancestry-path=") => {
                let spec = &s["--ancestry-path=".len()..];
                ancestry_path = true;
                match repo.rev_parse_single(spec.as_bytes()) {
                    Ok(id) => ancestry_bottoms.push(id.detach()),
                    Err(_) => {
                        // `handle_revision_opt()` returns `error()`, and
                        // `setup_revisions()` turns a negative return into
                        // `exit(128)` rather than the 129 a parse-options failure
                        // would give.
                        eprintln!(
                            "error: could not get commit for --ancestry-path argument {spec}"
                        );
                        return Ok(ExitCode::from(128));
                    }
                }
            }
            // `--simplify-by-decoration`: `simplify_commit()` keeps a decorated commit,
            // and — since simplification may not change the shape of the history — a
            // root or a merge; everything else is walked past.
            "--simplify-by-decoration" => simplify_by_decoration = true,
            "--show-pulls" => show_pulls = true,
            "-g" | "--walk-reflogs" => walk_reflogs = true,
            "--exclude-first-parent-only" => exclude_first_parent_only = true,
            "--bisect" => {
                if let Err(e) = seed_bisect_refs(&repo, negate, &mut seeds, &mut pending) {
                    return Ok(fatal_text(&e));
                }
                rev_input_given = true;
                bisect = true;
            }
            // `--bisect-all` and `--bisect-vars` are read by `cmd_rev_list()`'s own
            // leftover loop, *after* `setup_revisions()` — so unlike `--bisect`,
            // neither is a revision pseudo-option and neither seeds anything from
            // `refs/bisect/*`. They only ask for the search over whatever range the
            // rest of the command line named.
            "--bisect-all" => {
                bisect = true;
                bisect_all = true;
            }
            "--bisect-vars" => {
                bisect = true;
                bisect_vars = true;
            }
            // `revs->dense` (revision.c:2462-2465). `--dense` restores the
            // `repo_init_revisions()` default, so it is only ever an undo of an
            // earlier `--sparse`. Neither says anything without a pathspec: the
            // flag is read where `try_to_simplify_commit()` runs.
            // `OPT_CALLBACK_F(0, "date", …, parse_opt_date_mode)` in
            // `handle_revision_opt()`: the mode is a `parse_options` argument, so
            // both `--date=<mode>` and `--date <mode>` reach it, and an unknown one
            // is `die("unknown date format %s")` — 128, not the 129 a usage error
            // would give.
            "--date" => {
                let v = args.get(i + 1).cloned().unwrap_or_default();
                i += 1;
                match super::log::parse_date_mode(&v) {
                    Some(m) => date_mode = m,
                    None => return Ok(fatal(&format!("unknown date format {v}"))),
                }
            }
            s if s.starts_with("--date=") => {
                let v = &s["--date=".len()..];
                match super::log::parse_date_mode(v) {
                    Some(m) => date_mode = m,
                    None => return Ok(fatal(&format!("unknown date format {v}"))),
                }
            }
            "--sparse" => dense = false,
            "--dense" => dense = true,
            // ```c
            // } else if (!strcmp(arg, "--remove-empty")) {
            //         revs->remove_empty_trees = 1;
            // }
            // ```
            //
            // (revision.c.) Read where `try_to_simplify_commit()` compares a
            // parent, so like `--sparse`/`--dense` it says nothing without a
            // pathspec.
            "--remove-empty" => remove_empty = true,
            // ```c
            // } else if (!strcmp(arg, "--full-history")) {
            //         revs->simplify_history = 0;
            // }
            // ```
            //
            // (revision.c.) Nothing else: the walk is unchanged, and what changes
            // is that `try_to_simplify_commit()` stops pruning a merge's parent
            // list onto the first parent it is TREESAME to.
            "--full-history" => full_history = true,
            // ```c
            // } else if (!strcmp(arg, "--simplify-merges")) {
            //         revs->simplify_merges = 1;
            //         revs->topo_order = 1;
            //         revs->rewrite_parents = 1;
            //         revs->simplify_history = 0;
            //         revs->limited = 1;
            // }
            // ```
            //
            // (revision.c.) `topo_order` alone, so a `--date-order` already given
            // keeps its `sort_order` and stays date-topo; only the default date
            // order is upgraded.
            "--simplify-merges" => {
                simplify_merges_opt = true;
                full_history = true;
                if order == Order::Date {
                    order = Order::Topo;
                }
            }
            // ```c
            // } else if (!strcmp(arg, "--graph")) {
            //         graph_clear(revs->graph);
            //         revs->graph = graph_init(revs);
            // }
            // ```
            //
            // (revision.c.) Setting up the graph also turns on `revs->topo_order`
            // and `revs->rewrite_parents`. Both are observable in stock: on a
            // history where the two orders differ, `rev-list --graph --all` answers
            // in the `--topo-order` sequence and not the date one; and `rev-list
            // --graph --children` dies with `options '--parents' and '--children'
            // cannot be used together`, which is the `rewrite_parents && children`
            // check.
            "--graph" => {
                graph = true;
                if order == Order::Date {
                    order = Order::Topo;
                }
            }
            // ```c
            // } else if (!strcmp(arg, "--reflog")) {
            //         add_reflogs_to_pending(revs, *flags);
            // ```
            //
            // (revision.c:2766-2767.) A pseudo-option like the ref selectors, and
            // it lands in the same pending list at the same argv position — but it
            // reads no pattern, so the `--exclude` patterns standing beside it are
            // neither applied nor cleared. `*flags` is what `--not` holds, which is
            // also what cancels a `--no-walk` seen earlier (the shared check at the
            // end of this loop does that).
            "--reflog" => {
                // `add_reflogs_to_pending()` fills its `all_refs_cb` by hand rather
                // than through `init_all_refs_cb()`, so it never sets
                // `rev_input_given`: in a repository with no reflogs at all it pends
                // nothing, and `cmd_rev_list()`'s empty-pending check is then the
                // usage block rather than an empty listing.
                for id in super::shortlog::reflog_pending(&repo)? {
                    seeds.push(Seed {
                        id,
                        uninteresting: negate,
                        symmetric_left: false,
                        bottom: negate,
                    });
                    rev_input_given = true;
                }
            }
            // `revs->encode_email_headers` (revision.c:2526-2529).
            // `builtin/rev-list.c`'s `struct pretty_print_context ctx = {0}` never
            // copies it, so — unlike `log` — the flag changes nothing this command
            // prints. It is still `setup_revisions()`'s option and must be
            // accepted; see [`super::log::EmailStyle::REV_LIST`].
            "--encode-email-headers" | "--no-encode-email-headers" => {}
            // ```c
            // } else if (!strcmp(arg, "--single-worktree")) {
            //         revs->single_worktree = 1;
            // ```
            //
            // (`revision.c:2903-2904`.) The flag only decides whether `--all` and
            // `--reflog` reach into *other* worktrees' HEADs; this port reads the
            // main ref store either way, so setting it changes nothing here. It is
            // still `setup_revisions()`'s option and rejecting it is a refusal git
            // never prints.
            "--single-worktree" => {}
            "--topo-order" => order = Order::Topo,
            "--date-order" => order = Order::DateTopo,
            "--author-date-order" => order = Order::AuthorDateTopo,
            "--merges" => min_parents = 2,
            "--no-merges" => max_parents = Some(1),
            "--no-min-parents" => min_parents = 0,
            "--no-max-parents" => max_parents = None,
            "-q" | "--quiet" => quiet = true,
            "--commit-header" => include_header = true,
            "--no-commit-header" => include_header = false,
            "--header" => verbose_header = true,
            "--timestamp" => show_timestamp = true,
            "--not" => negate = !negate,
            // `--encoding=<enc>`, which the pretty formats take just as `log` does.
            s if s.starts_with("--encoding=") => {
                let v = &s["--encoding=".len()..];
                if !super::blame::encoding_is_passthrough(v) {
                    eprintln!(
                        "fatal: unsupported option {s} (only utf-8 and none are ported)"
                    );
                    return Ok(ExitCode::from(128));
                }
            }
            "--no-walk" => no_walk = true,
            "--do-walk" => no_walk = false,
            // ```c
            // if (!strcmp(arg, "--stdin")) {
            //         if (revs->disable_stdin) { … continue; }
            //         if (revs->read_from_stdin++)
            //                 die("--stdin given twice?");
            //         read_revisions_from_stdin(revs, &prune_data);
            //         continue;
            // }
            // ```
            //
            // (`revision.c:3047-3057`.) The read happens *here*, in the middle of
            // the argument scan, so the lines are spliced in at this position and
            // the scan carries on over them with whatever `--not` currently holds.
            "--stdin" => {
                if read_stdin {
                    return Ok(fatal("--stdin given twice?"));
                }
                read_stdin = true;
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text)?;
                let mut lines: Vec<String> = Vec::new();
                let mut kinds: Vec<Origin> = Vec::new();
                let mut seen_end_of_options = false;
                let mut rest = text.lines();
                for line in rest.by_ref() {
                    // `strbuf_getline()` strips a trailing CR of its own.
                    let line = line.strip_suffix('\r').unwrap_or(line);
                    // `if (!sb.len) break;` — an empty line ends the *whole* read,
                    // pathspecs included, rather than being skipped.
                    if line.is_empty() {
                        break;
                    }
                    if line == "--" {
                        // `seen_dashdash = 1; break;` then
                        // `read_pathspec_from_stdin()`: every remaining line is a
                        // pathspec, empty ones included.
                        pathspecs.extend(rest.map(|p| p.as_bytes().to_vec()));
                        break;
                    }
                    if !seen_end_of_options && line == "--end-of-options" {
                        seen_end_of_options = true;
                        continue;
                    }
                    lines.push(line.to_string());
                    kinds.push(if seen_end_of_options {
                        Origin::StdinAfterEndOfOptions
                    } else {
                        Origin::Stdin
                    });
                }
                // `read_revisions_from_stdin()`'s `int flags = 0;` is its own, so
                // the block starts with `--not` cleared and the argv scan gets its
                // state back at the sentinel.
                lines.push(String::new());
                kinds.push(Origin::StdinEnd(negate));
                negate = false;
                argv.splice(i + 1..i + 1, lines);
                origin.splice(i + 1..i + 1, kinds);
            }
            "-i" | "--regexp-ignore-case" => ignore_case = true,
            "-E" | "--extended-regexp" => dialect = Dialect::Extended,
            "-F" | "--fixed-strings" => dialect = Dialect::Fixed,
            "-P" | "--perl-regexp" => dialect = Dialect::Perl,
            "--basic-regexp" => dialect = Dialect::Basic,
            "--all-match" => all_match = true,
            "--invert-grep" => invert_grep = true,
            "--no-filter" => filter = None,
            "--pretty" => {
                verbose_header = true;
                pretty = Some(Pretty::Medium);
            }
            // `--oneline` is git`s `--pretty=oneline --abbrev-commit`, and
            // `--abbrev-commit`/`--no-abbrev-commit` shorten the object name on
            // their own (`rev_list.c` shares `cmd_log_init`s parser here).
            "--oneline" => {
                verbose_header = true;
                pretty = Some(Pretty::Oneline);
                abbrev_commit = true;
            }
            "--abbrev-commit" => abbrev_commit = true,
            "--no-abbrev-commit" => abbrev_commit = false,
            // `revs->abbrev` (revision.c:2639-2653). It is the *minimum* width
            // `repo_find_unique_abbrev()` is asked for, clamped to
            // `[MINIMUM_ABBREV, hexsz]`; `--no-abbrev` is git's zero, and
            // `builtin/rev-list.c:277-282` prints the whole id unless
            // `--abbrev-commit` and a non-zero `revs->abbrev` are both set — so
            // `--abbrev=8` on its own changes nothing here.
            "--no-abbrev" => abbrev_len = Some(0),
            "--abbrev" => abbrev_len = None,
            s if s.starts_with("--abbrev=") => {
                abbrev_len = Some(crate::abbrev::parse_abbrev_arg(
                    &s["--abbrev=".len()..],
                    repo.object_hash().len_in_hex(),
                ));
            }
            "-n" => {
                i += 1;
                let Some(n) = argv.get(i) else {
                    eprintln!("error: -n requires an argument");
                    return Ok(ExitCode::from(128));
                };
                match parse_git_int(n) {
                    Some(v) => max_count = clamp_count(v),
                    None => return Ok(not_an_integer(n)),
                }
            }
            "--" => {
                // Everything after `--` is a pathspec, never a rev or flag.
                pathspecs.extend(argv[i + 1..].iter().map(|s| s.as_bytes().to_vec()));
                break;
            }
            // `--exclude=<glob>` accumulates until the next ref-selecting option
            // consumes and clears it (`clear_ref_exclusions`); anything else in
            // between leaves the accumulation alone.
            "--exclude" => {
                i += 1;
                let Some(v) = argv.get(i) else {
                    eprintln!("error: option 'exclude' requires a value");
                    return Ok(usage_error());
                };
                ref_excludes.push(v.clone());
            }
            s if s.starts_with("--exclude=") => {
                ref_excludes.push(s["--exclude=".len()..].to_string());
            }
            "--indexed-objects" => {
                if let Err(e) = seed_index_objects(&repo, negate, &mut pending) {
                    return Ok(fatal(&e));
                }
            }
            // ```c
            // } else if (!strcmp(arg, "--alternate-refs")) {
            //         add_alternate_refs_to_pending(revs, *flags);
            // ```
            //
            // (`revision.c:2904-2905`.) `add_one_alternate_ref()`
            // (`revision.c:1866-1876`) queues each id `odb_for_each_alternate_ref()`
            // yields with `get_reference()` + `add_pending_object()`, under the
            // hex text as its name — so an annotated tag tip is peeled by the
            // walk, exactly as a `<rev>` operand would be, and carries the flags
            // `--not` holds at this argv position.
            "--alternate-refs" => {
                for id in crate::alternate_refs::tips(&repo) {
                    if let Some(id) = peel_recording_tags(&repo, id, &mut pending) {
                        seeds.push(Seed {
                            id,
                            uninteresting: negate,
                            symmetric_left: false,
                            bottom: negate,
                        });
                    }
                }
            }
            s if s.starts_with("--exclude-hidden=") => {
                // `if (exclusions->hidden_refs_configured) die(…)` — the flag is
                // set by the *config walk*, so a second `--exclude-hidden=` is
                // refused only when the first one actually found a pattern.
                if hidden_configured {
                    return Ok(fatal("--exclude-hidden= passed more than once"));
                }
                match hidden_ref_patterns(&repo, &s["--exclude-hidden=".len()..]) {
                    Ok(patterns) => {
                        hidden_configured = true;
                        hidden_refs = patterns;
                    }
                    Err(e) => return Ok(fatal(&e)),
                }
            }
            "--glob" => {
                i += 1;
                let Some(v) = argv.get(i) else {
                    eprintln!("error: option 'glob' requires a value");
                    return Ok(usage_error());
                };
                let sel = super::log::RefSelection::new(
                    0,
                    super::log::RefSelector::Glob,
                    Some(v),
                    std::mem::take(&mut ref_excludes),
                    negate,
                );
                if let Err(e) = seed_ref_set(&repo, &sel, negate, &hidden_refs, &mut seeds, &mut pending) {
                    return Ok(fatal_text(&e));
                }
                rev_input_given = true;
            }
            s if super::log::ref_selector(s).is_some() => {
                // Seeded in place: git processes a ref-set selector where it
                // appears, and with every commit sharing a timestamp the seed
                // order is what decides the output order.
                let (kind, pattern) = super::log::ref_selector(s).expect("checked above");
                // `handle_revision_pseudo_opt()` refuses the three *narrowed*
                // selectors once `--exclude-hidden=` has been seen, because their
                // callback is handed a trimmed name that a `refs/…` hideRefs
                // pattern could never match. `--all` and `--glob=` are not
                // refused; they see the full name.
                if hidden_configured {
                    if let Some(name) = match kind {
                        super::log::RefSelector::Branches => Some("--branches"),
                        super::log::RefSelector::Tags => Some("--tags"),
                        super::log::RefSelector::Remotes => Some("--remotes"),
                        _ => None,
                    } {
                        eprintln!(
                            "error: options '--exclude-hidden' and '{name}' cannot be used together"
                        );
                        return Ok(usage_error());
                    }
                }
                let sel = super::log::RefSelection::new(
                    0,
                    kind,
                    pattern,
                    std::mem::take(&mut ref_excludes),
                    negate,
                );
                if let Err(e) = seed_ref_set(&repo, &sel, negate, &hidden_refs, &mut seeds, &mut pending) {
                    return Ok(fatal_text(&e));
                }
                rev_input_given = true;
            }
            s if s.starts_with("--pretty=") || s.starts_with("--format=") => {
                let spec = s.split_once('=').expect("checked above").1;
                verbose_header = true;
                match get_commit_format(Some(&repo), spec) {
                    Ok(Some((p, _))) => pretty = Some(p),
                    // A value that names no known format and carries no `%` is
                    // what git reports as an invalid `--pretty` argument.
                    Ok(None) => return Ok(fatal(&format!("invalid --pretty format: {spec}"))),
                    Err(e) => return Ok(fatal(&e.to_string())),
                }
            }
            s if s.starts_with("--disk-usage") => {
                match &s["--disk-usage".len()..] {
                    "" => {}
                    "=human" => disk_usage_human = true,
                    v if v.starts_with('=') => {
                        return Ok(fatal(&format!(
                            "invalid value for '--disk-usage=<format>': '{}', the only allowed format is 'human'",
                            &v[1..]
                        )))
                    }
                    _ => return Ok(usage_error()),
                }
                disk_usage = true;
                quiet = true;
            }
            s if s.starts_with("--no-walk=") => {
                match &s["--no-walk=".len()..] {
                    "sorted" => unsorted_input = false,
                    "unsorted" => unsorted_input = true,
                    // `handle_revision_pseudo_opt` returns the error to
                    // `setup_revisions`, which then prints the usage block too.
                    _ => {
                        eprintln!("error: invalid argument to --no-walk");
                        return Ok(usage_error());
                    }
                }
                no_walk = true;
            }
            // `parse_missing_action_value()` (builtin/rev-list.c) clears
            // `fetch_if_missing` for every action but `error`: an action that
            // *reports* an absence must not repair it on the way past, or the
            // walk changes what it is measuring. `MA_ERROR` deliberately keeps
            // the lazy fetch on, which is why `--missing=error` succeeds in a
            // partial clone rather than dying on the objects the clone skipped.
            s if s.starts_with("--missing=") => match &s["--missing=".len()..] {
                "allow-any" => {
                    gix::odb::store::set_fetch_if_missing(false);
                    missing = Missing::AllowAny;
                }
                "print" => {
                    gix::odb::store::set_fetch_if_missing(false);
                    missing = Missing::Print;
                }
                // ```c
                // if (!strcmp(value, "print-info")) {
                //         arg_missing_action = MA_PRINT_INFO;
                //         fetch_if_missing = 0;
                //         return 1;
                // }
                // ```
                //
                // (`parse_missing_action_value()`, builtin/rev-list.c:523-527.)
                // It records the same objects `print` does; the difference is
                // only in how [`print_missing_object`] renders them.
                "print-info" => {
                    gix::odb::store::set_fetch_if_missing(false);
                    missing = Missing::PrintInfo;
                }
                "allow-promisor" => {
                    gix::odb::store::set_fetch_if_missing(false);
                    missing = Missing::AllowPromisor;
                }
                // git leaves an unrecognised value on the default action.
                _ => missing = Missing::Error,
            },
            s if s.starts_with("--filter=") => match parse_filter(&s["--filter=".len()..]) {
                Some(f) => filter = Some(f),
                None => {
                    return Ok(fatal(&format!(
                        "invalid filter-spec '{}'",
                        &s["--filter=".len()..]
                    )))
                }
            },
            // `--skip=<n>` / `--skip <n>`: `revs->skip_count`. A negative count is
            // git's "no skip" (its `>= 0` guard), and a non-numeral is
            // `setup_revisions()`'s `die("'%s': not an integer")` rather than the
            // usage block.
            "--skip" => {
                i += 1;
                let Some(v) = argv.get(i) else {
                    return Ok(usage_error());
                };
                match parse_git_int(v) {
                    Some(n) => skip_count = n.max(0) as usize,
                    None => return Ok(not_an_integer(v)),
                }
            }
            s if s.starts_with("--skip=") => {
                let v = &s["--skip=".len()..];
                match parse_git_int(v) {
                    Some(n) => skip_count = n.max(0) as usize,
                    None => return Ok(not_an_integer(v)),
                }
            }
            // `if (revs.show_notes) die(_("rev-list does not support display of
            // notes"));` (builtin/rev-list.c) — every spelling that turns notes on
            // is fatal, and the ones that turn them off are accepted and inert.
            "--notes" | "--show-notes" | "--standard-notes" => {
                return Ok(fatal("rev-list does not support display of notes"));
            }
            s if s.starts_with("--notes=") || s.starts_with("--show-notes=") => {
                return Ok(fatal("rev-list does not support display of notes"));
            }
            "--no-notes" | "--no-standard-notes" => {}
            s if s.starts_with("--max-count=") => {
                let v = &s["--max-count=".len()..];
                match parse_git_int(v) {
                    Some(n) => max_count = clamp_count(n),
                    None => return Ok(not_an_integer(v)),
                }
            }
            s if s.starts_with("--min-parents=") => {
                let v = &s["--min-parents=".len()..];
                match parse_git_int(v) {
                    Some(n) => min_parents = n,
                    None => return Ok(not_an_integer(v)),
                }
            }
            s if s.starts_with("--max-parents=") => {
                let v = &s["--max-parents=".len()..];
                match parse_git_int(v) {
                    // git stores a negative max-parents as -1: "no upper limit".
                    Some(n) => max_parents = if n < 0 { None } else { Some(n as usize) },
                    None => return Ok(not_an_integer(v)),
                }
            }
            s if s.len() > 2
                && s.starts_with("-n")
                && s[2..].bytes().all(|b| b.is_ascii_digit()) =>
            {
                match parse_git_int(&s[2..]) {
                    Some(n) => max_count = clamp_count(n),
                    None => return Ok(not_an_integer(&s[2..])),
                }
            }
            // ```c
            // } else if (*arg == '-' && isdigit(arg[1])) {
            //         revs->max_count = atoi(arg + 1);
            //         revs->no_walk = 0;
            // ```
            //
            // (revision.c:2743-2746.) `atoi()` stops at the first non-digit and
            // answers 0 rather than failing, so `-1x` is `-0`; only the second
            // character has to be a digit for the branch to be taken at all.
            s if s.len() > 1 && s.starts_with('-') && s.as_bytes()[1].is_ascii_digit() => {
                let digits: String = s[1..].chars().take_while(char::is_ascii_digit).collect();
                max_count = clamp_count(digits.parse::<i64>().unwrap_or(i64::MAX));
                no_walk = false;
            }
            // Every remaining flag is one git knows and this does not; a
            // revision never starts with `-`, so anything left is a usage error.
            s if s.starts_with('-') => return Ok(usage_error()),
            // `handle_revision_arg_1()`'s guard ahead of `handle_dotdot()`: a
            // bare `..` is the pathspec for the parent directory, never
            // `HEAD..HEAD`. `setup_revisions()` then sends it to
            // `append_prune_data()`, and the pathspec layer rejects it for
            // leaving the repository. See
            // [`crate::objname::is_parent_directory_pathspec`].
            s if origin[i] == Origin::Argv
                && crate::objname::is_parent_directory_pathspec(s, seen_dashdash) =>
            {
                pathspecs.push(s.as_bytes().to_vec());
            }
            s => {
                // Everything `get_oid_basic()` writes for an operand read off
                // argv, in git's order and with `handle_dotdot_1()`'s `||`
                // short-circuit across a range's endpoints. `rev-list` runs the
                // same `setup_revisions()` as `log`, so the rule is shared rather
                // than restated.
                //
                // `read_revisions_from_stdin()` brackets its loop with
                // `cfg->warn_on_object_refname_ambiguity = 0`, which gates
                // `get_oid_basic()`'s *full-hex* branch alone — a plain refname on
                // stdin still warns, and does so at this position.
                let from_stdin = origin[i] != Origin::Argv;
                super::log::warn_operand(&repo, s, !from_stdin);
                if let Err(e) = seed_revision(
                    &repo,
                    s,
                    negate,
                    seen_dashdash || from_stdin,
                    &mut seeds,
                    &mut pending,
                ) {
                    return Ok(fatal_text(&e));
                }
                note_parsed(&repo, s, &seeds[seeds_before..], &mut parsed_commits)?;
                rev_input_given = true;
                reflog_names.push(s.to_string());
            }
        }
        // `add_pending_object_with_path()` clears `revs->no_walk` the moment an
        // object carrying UNINTERESTING joins the pending list, which is
        // `git-rev-list(1)`'s "This has no effect if a range is specified": every
        // spelling that excludes — `^<rev>`, the left side of `<a>..<b>`, the merge
        // bases of `<a>...<b>`, anything after `--not`, `--branches`/`--tags`/`--all`
        // while `--not` is in force — cancels a `--no-walk` seen before it. Both are
        // positional, so a `--no-walk` written afterwards turns walking off again.
        if seeds[seeds_before..].iter().any(|s| s.uninteresting) {
            no_walk = false;
        }
        i += 1;
    }

    // `want_ancestry()` is `revs->rewrite_parents || revs->children.name`, and the
    // die reads the first as `--parents` however it was turned on — which `--graph`
    // does.
    if (show_parents || graph) && show_children {
        return Ok(fatal(
            "options '--parents' and '--children' cannot be used together",
        ));
    }
    if graph && reverse {
        return Ok(fatal(
            "options '--graph' and '--reverse' cannot be used together",
        ));
    }
    if graph && no_walk {
        return Ok(fatal(
            "options '--no-walk' and '--graph' cannot be used together",
        ));
    }
    // The graph draws one block per commit record. `--objects` interleaves object
    // names between those blocks unprefixed, and `--count`/`--quiet`/`--disk-usage`
    // draw the rows with no record at all; neither shape is rendered here, so those
    // combinations keep the refusal they already had rather than printing something
    // that is not what stock prints.
    if graph && (objects || count_only || quiet || disk_usage) {
        return Ok(usage_error());
    }

    // `parse_pathspec()` runs inside `setup_revisions()`, so a rejected element
    // is fatal here — before the walk, and on the paths that never build a
    // matcher at all.
    if let Some(msg) = crate::pathspec::parse_pathspec_fatal(&repo, &pathspecs) {
        eprintln!("fatal: {msg}");
        return Ok(ExitCode::from(128));
    }

    // `revs->abbrev` is the minimum width every abbreviation in the run is asked
    // for, so it goes in front of the same `core.abbrev` lookup gitoxide already
    // makes. Zero (`--no-abbrev`) is handled at the print site instead — it turns
    // abbreviation off rather than choosing a width.
    if let Some(n) = abbrev_len.filter(|n| *n > 0) {
        let mut config = repo.config_snapshot_mut();
        config.append_config(Some(format!("core.abbrev={n}")), gix::config::Source::Cli)?;
        config.commit()?;
    }

    if filter.is_some() && !objects {
        return Ok(fatal("object filtering requires --objects"));
    }

    let mut tips: Vec<ObjectId> = seeds
        .iter()
        .filter(|s| !s.uninteresting)
        .map(|s| s.id)
        .collect();
    let hidden: Vec<ObjectId> = seeds
        .iter()
        .filter(|s| s.uninteresting)
        .map(|s| s.id)
        .collect();

    // git treats "nothing to walk from" as a usage error, not a fatal one —
    // except under `--objects`, which asks for an object listing and is content
    // to produce an empty one, under `--stdin`, which was given its input, and
    // when a revision *was* named but selected nothing (`--not main --all`).
    // `(!(revs.tag_objects || revs.tree_objects || revs.blob_objects) &&
    //   !revs.pending.nr)` — the pending list is consulted before
    // `prepare_revision_walk()` drops the non-commits, so
    // `git rev-list --indexed-objects` (which names no revision at all) is not a
    // usage error.
    if tips.is_empty() && !objects && !read_stdin && !rev_input_given && pending.is_empty() {
        return Ok(usage_error());
    }
    dedup_in_place(&mut tips);

    // 1. Full commit list in date order — the input every later stage refines.
    let mut commits: Vec<ObjectId> = Vec::new();
    let mut parents_of: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    // The read failure that ended the walk, if one did — raised where git's own
    // `die()` fires, which is after the commits it had already streamed. See
    // [`super::log::WalkAbort`].
    let mut abort: Option<super::log::WalkAbort> = None;
    if no_walk {
        // `prepare_revision_walk` returns before `limit_list` under `--no-walk`,
        // so the list is exactly the pending commits, deduplicated by the SEEN
        // flag `handle_commit()` sets as it reads them — first occurrence wins,
        // and an id first pended UNINTERESTING keeps that flag however it is
        // named again. `<a>...<b>` pends the merge bases ahead of both endpoints,
        // which is why `git rev-list --no-walk a...b` drops an endpoint that *is*
        // a merge base.
        let excluded = super::log::no_walk_uninteresting(&repo, &hidden, &parsed_commits);
        let mut pending: Vec<ObjectId> = Vec::new();
        for seed in &seeds {
            if !pending.contains(&seed.id) {
                pending.push(seed.id);
            }
        }
        commits = pending.into_iter().filter(|id| !excluded.contains(id)).collect();
        // `commit_list_sort_by_date()` is a *stable* mergesort (`mergesort.h`:
        // "Take from `list` on equality"), so a date tie keeps the pending order.
        if !unsorted_input {
            let dates: HashMap<ObjectId, i64> = commits
                .iter()
                .map(|id| (*id, commit_date(&repo, *id)))
                .collect();
            commits.sort_by_key(|id| std::cmp::Reverse(dates[id]));
        }
        for id in &commits {
            parents_of.insert(*id, commit_parents(&repo, *id));
        }
    } else if !tips.is_empty() {
        let mut platform = repo
            .rev_walk(tips.clone())
            .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
        if first_parent {
            platform = platform.first_parent_only();
        }
        // `--exclude-first-parent-only` stops `mark_parents_uninteresting()` at
        // each excluded commit's first parent, which `with_hidden` cannot express
        // — it paints every parent. So the closure is computed here and the walk
        // is left unhidden, with the marked commits dropped from its result below.
        if !hidden.is_empty() && !exclude_first_parent_only {
            platform = platform.with_hidden(hidden.clone());
        }
        for info in platform.all()? {
            let info = match info {
                Ok(info) => info,
                // `process_parents()` (revision.c:1189-1194) parses every parent of
                // the commit it just popped, so a parent the odb cannot produce ends
                // the walk one commit *earlier* than this iterator notices it — the
                // iterator queues parent ids unread and only trips when one is
                // popped. `locate` puts the abort back where git has it.
                Err(err) => match super::log::WalkAbort::locate(&repo, &commits, &parents_of) {
                    Some((at, found)) => {
                        commits.truncate(at);
                        abort = Some(found);
                        break;
                    }
                    None => return Err(err.into()),
                },
            };
            parents_of.insert(info.id, info.parent_ids.to_vec());
            commits.push(info.id);
        }
    }

    if exclude_first_parent_only && !hidden.is_empty() {
        let excluded = super::log::ancestor_closure_opt(&repo, &hidden, true)?;
        commits.retain(|id| !excluded.contains(id));
    }

    // The reflog walk replaces the list wholesale: each entry is one "commit" in
    // the order `git log -g` reports them, and every filter below then applies to
    // that list exactly as it would to an ancestry walk.
    if walk_reflogs {
        if reflog_names.is_empty() {
            reflog_names.push("HEAD".to_owned());
        }
        let nodes = super::log::reflog_walk(&repo, &reflog_names)?;
        commits = nodes.iter().map(|n| n.id).collect();
        parents_of = nodes.iter().map(|n| (n.id, n.parents.clone())).collect();
        abort = None;
    }

    // `--since` is git's `max_age`: a commit older than the bound is marked
    // UNINTERESTING, which prunes everything it reaches as well. `--until`
    // (`min_age`) only skips the commit itself and keeps walking.
    if let Some(bound) = max_age {
        let stale: Vec<ObjectId> = commits
            .iter()
            .copied()
            .filter(|id| commit_date(&repo, *id) < bound)
            .collect();
        if !stale.is_empty() {
            let pruned = reachable_from(&stale, &parents_of);
            commits.retain(|id| !pruned.contains(id));
        }
    }
    if let Some(bound) = min_age {
        commits.retain(|id| commit_date(&repo, *id) <= bound);
    }
    if let Some(bound) = max_age_as_filter {
        commits.retain(|id| commit_date(&repo, *id) >= bound);
    }

    // `SYMMETRIC_LEFT` reaches every ancestor of the left tip: `process_parents`
    // ORs it onto each parent it walks through, so membership is reachability.
    let left_tips: Vec<ObjectId> = seeds
        .iter()
        .filter(|s| s.symmetric_left && !s.uninteresting)
        .map(|s| s.id)
        .collect();
    let left = reachable_from(&left_tips, &parents_of);

    // `mark_uninteresting` over the promisor packs (revision.c:4001-4003) runs
    // before the walk, so a commit a promisor pack holds never enters the list at
    // all — and neither does anything reachable only through it.
    if exclude_promisor {
        let promisor = promisor_pack_objects(&repo);
        commits.retain(|id| !promisor.contains(id));
    }

    // `cherry_pick_list()` (revision.c): with commits on both sides of a symmetric difference,
    // the two sides are compared by *patch id* and every commit whose change appears on the other
    // side is marked `PATCHSAME`. git computes the ids for the smaller side and looks the larger
    // side up in that table — the same work `git cherry` does, through the same
    // `commit_patch_id()`.
    let patch_same: HashSet<ObjectId> = if cherry_mark || cherry_pick {
        cherry_pick_list(&repo, &commits, &left)?
    } else {
        HashSet::new()
    };
    // `if (revs->cherry_pick && (commit->object.flags & PATCHSAME)) continue;`: `--cherry-pick`
    // without `--cherry-mark` drops the equivalent commits instead of marking them.
    if cherry_pick && !cherry_mark {
        commits.retain(|id| !patch_same.contains(id));
    }
    // `--left-only` / `--right-only` keep one side of the difference.
    if left_only {
        commits.retain(|id| left.contains(id));
    }
    if right_only {
        commits.retain(|id| !left.contains(id));
    }

    // `--ancestry-path`: keep only the commits that descend from an excluded tip.
    if ancestry_path {
        // `collect_bottom_commits()` runs only for the argument-less spelling
        // (`ancestry_path_implicit_bottoms`, revision.c:1448-1453), so a
        // `--ancestry-path=<commit>` needs no range to have excluded anything.
        let bottoms: Vec<ObjectId> = match ancestry_bottoms.is_empty() {
            false => ancestry_bottoms.clone(),
            true => seeds.iter().filter(|s| s.bottom).map(|s| s.id).collect(),
        };
        if bottoms.is_empty() {
            return Ok(fatal(
                "--ancestry-path given but there are no bottom commits",
            ));
        }
        commits = limit_to_ancestry(&bottoms, &commits, &parents_of);
    }

    // The path limit is applied where `try_to_simplify_commit` runs — inside the
    // walk, before the ordering and the output-time filters — because collapsing
    // a TREESAME merge onto one parent changes both the topological sort and the
    // `--children` map that follow.
    let mut treesame: HashSet<ObjectId> = HashSet::new();
    // `rewrite_parents()`'s answer for every shown commit, when `--simplify-merges`
    // produced it. The pass belongs to the *display*, so it is applied where git
    // applies it — in `simplify_commit()`, after `get_commit_action()` has read the
    // parent list `simplify_one()` left behind.
    let mut simplified_display: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    if !pathspecs.is_empty() {
        let mut specs = super::log::PathspecMatcher::new(&repo, &pathspecs)?;
        // `--remove-empty` runs ahead of the TREESAME classification below because
        // what it does is cut a parent's ancestry off the walk: the commits it
        // removes are ones `limit_list()` never reached, so they are never
        // classified and never counted anywhere downstream.
        if remove_empty {
            let roots = empty_tree_roots(
                &repo,
                &commits,
                &parents_of,
                first_parent,
                dense,
                !full_history,
                &mut specs,
            )?;
            if !roots.is_empty() {
                for root in &roots {
                    parents_of.insert(*root, Vec::new());
                }
                let reachable = reachable_from(&tips, &parents_of);
                commits.retain(|id| reachable.contains(id));
            }
        }
        if full_history {
            // `--full-history` clears `revs->simplify_history`, so
            // `try_to_simplify_commit()` compares every parent, records a
            // per-parent TREESAME decoration and leaves `commit->parents` alone.
            // No side of a merge is walked past, which is why nothing is dropped
            // for unreachability here. The shared port of the three revision.c
            // passes is [`super::simplify`]; `log`, `shortlog` and `whatchanged`
            // read the same one, which is the point of it being shared.
            //
            // Relevance is `relevant_commit()` — `!(flags & UNINTERESTING)` — and
            // an excluded commit never reaches this list, so the walked set is
            // exactly the relevant one.
            let walked: HashSet<ObjectId> = commits.iter().copied().collect();
            let mode = super::simplify::Mode {
                dense,
                simplify_history: false,
                first_parent,
            };
            let mut diff = PathDiff { repo: &repo, specs: &mut specs };
            let mut info: HashMap<ObjectId, super::simplify::Classified> =
                HashMap::with_capacity(commits.len());
            for id in &commits {
                let parents = parents_of.get(id).map(Vec::as_slice).unwrap_or(&[]);
                info.insert(
                    *id,
                    super::simplify::classify(*id, parents, &walked, mode, &mut diff)?,
                );
            }
            if simplify_merges_opt {
                // `simplify_merges()` prunes `revs->commits` to the commits that
                // simplify to themselves, and it runs inside `limit_list()` — so
                // the survivors are what `set_children()` and every later stage
                // see. `get_commit_action()` then applies its own TREESAME filter
                // on top, with `want_ancestry()` true because `--simplify-merges`
                // sets `revs->rewrite_parents`.
                let sm =
                    super::simplify::merge_simplify(&repo, &commits, &info, first_parent)?;
                let ancestry = super::simplify::Ancestry {
                    walked: &walked,
                    treesame: &sm.treesame,
                    parents: &sm.parents,
                    first_parent,
                };
                let kept: HashSet<ObjectId> = commits
                    .iter()
                    .copied()
                    .filter(|id| {
                        sm.kept(id)
                            && super::simplify::shows(
                                sm.treesame.get(id).copied().unwrap_or(false),
                                sm.parents.get(id).map(Vec::as_slice).unwrap_or(&[]),
                                &walked,
                                true,
                                dense,
                                true,
                            )
                    })
                    .collect();
                for id in &kept {
                    let parents = sm.parents.get(id).cloned().unwrap_or_default();
                    // `simplify_commit()` reaches `rewrite_parents()` only under
                    // `revs->prune && revs->dense`; `--sparse` prints the list
                    // `simplify_one()` produced.
                    let display =
                        if dense { ancestry.rewrite(&parents) } else { parents.clone() };
                    simplified_display.insert(*id, display);
                }
                // `simplify_one()` rewrote `commit->parents` in place, so the
                // parent-count filters and the `--children` map below read the
                // simplified list rather than the recorded one.
                let rewritten: Vec<(ObjectId, Vec<ObjectId>)> = sm.parents.into_iter().collect();
                parents_of.extend(rewritten);
                commits.retain(|id| kept.contains(id));
            } else {
                // `want_ancestry()` is `revs->rewrite_parents || revs->children.name`:
                // `--parents` sets the first, `--children` the second. With it,
                // `get_commit_action()` keeps a TREESAME merge between two or more
                // relevant commits, because that merge ties the topology together.
                let want_ancestry = show_parents || show_children;
                for id in &commits {
                    let Some(classified) = info.get(id) else { continue };
                    // `if (revs->show_pulls && (commit->object.flags & PULL_MERGE))
                    // return commit_show;`. PULL_MERGE is set where the *first*
                    // parent's comparison came out different — the merge that
                    // brought a change in rather than diverted around it.
                    if show_pulls && classified.treesame_with.first() == Some(&false) {
                        continue;
                    }
                    if !super::simplify::shows(
                        classified.treesame,
                        &classified.parents,
                        &walked,
                        true,
                        dense,
                        want_ancestry,
                    ) {
                        treesame.insert(*id);
                    }
                }
            }
        } else {
            let mut simplified: Vec<(ObjectId, Vec<ObjectId>)> = Vec::new();
            for id in &commits {
                let parents = parents_of.get(id).map(Vec::as_slice).unwrap_or(&[]);
                // `if (!revs->dense && !commit->parents->next) return;`
                // (revision.c:996): under `--sparse` a non-merge is never compared at
                // all, so it is never TREESAME, is always shown, and keeps its parent.
                // A root has no `parents->next` either but is compared earlier, at
                // revision.c:979-997.
                if !dense && parents.len() == 1 {
                    continue;
                }
                let same = treesame_parent(
                    &repo,
                    *id,
                    parents,
                    first_parent,
                    &mut specs,
                    &parents_of,
                    show_pulls,
                )?;
                match same {
                    None => {}
                    Some(parent) => {
                        // The `revs->prune && revs->dense` display gate
                        // (revision.c:4221) is what drops a TREESAME commit. Under
                        // `--sparse` it does not fire, so the commit stays even though
                        // git has marked it — what `--sparse` leaves in place is only
                        // the in-place parent prune below.
                        if dense {
                            treesame.insert(*id);
                        }
                        if let Some(parent) = parent {
                            if parents.len() > 1 {
                                simplified.push((*id, vec![parent]));
                            }
                        }
                    }
                }
            }
            parents_of.extend(simplified);
            // `process_parents` simplifies a commit *before* queueing its parents, so
            // the side of a collapsed merge is never walked at all. Re-deriving what
            // the simplified links still reach drops those commits here too, which is
            // what keeps them out of the `--children` map.
            let reachable = reachable_from(&tips, &parents_of);
            commits.retain(|id| reachable.contains(id));
        }
    }

    // 2. Reorder, 3. filter by parent count, 4. limit, 5. reverse — in that
    // order, because git sorts the whole list, then drops commits at output
    // time, and only counts what it actually emits against `--max-count`.
    // `prepare_revision_walk()` returns as soon as `revs->no_walk` survived
    // (revision.c:4009), which is *before* both `sort_in_topological_order()` and
    // `init_topo_walk()` — so `--topo-order` and `--date-order` are silently inert
    // under `--no-walk`, and the pending order (or its date sort) stands.
    if order != Order::Date && !no_walk {
        // `--topo-order`/`--date-order` make `prepare_revision_walk()` run
        // `limit_list()` and `sort_in_topological_order()` before it returns
        // (revision.c:4033-4039), so the failure is a setup failure and
        // `cmd_rev_list()` prints nothing at all.
        if let Some(abort) = abort {
            return Ok(abort.die_setup());
        }
        // `record_author_date()` (commit.c:866-891) reads the `author` header of
        // every listed commit up front; one without a parsable date keeps the
        // slab's zero and so sorts as the epoch rather than being dropped.
        let dates: Option<HashMap<ObjectId, i64>> = match order {
            Order::DateTopo => Some(commits.iter().map(|id| (*id, commit_date(&repo, *id))).collect()),
            Order::AuthorDateTopo => {
                Some(commits.iter().map(|id| (*id, author_date(&repo, *id))).collect())
            }
            Order::Date | Order::Topo => None,
        };
        // `--first-parent` narrows the sort's edges only when a commit-graph with
        // generation numbers is present: `prepare_revision_walk()` then picks
        // `init_topo_walk()`, which breaks after each commit's first parent, over
        // `sort_in_topological_order()`, which has no `rev_info` and counts every
        // parent (revision.c, commit.c).
        let first_parent = first_parent && repo.commit_graph().is_ok();
        let fp_parents: HashMap<ObjectId, Vec<ObjectId>> = match first_parent {
            true => parents_of
                .iter()
                .map(|(id, ps)| (*id, ps.iter().take(1).copied().collect()))
                .collect(),
            false => HashMap::new(),
        };
        let edges = if first_parent { &fp_parents } else { &parents_of };
        commits = topo_sort(&commits, edges, dates.as_ref());
    }

    // `--bisect` replaces the whole list with the one commit `find_bisection`
    // picks, before any output-time filter runs. Under `--bisect-all` the list
    // survives whole, reordered by the search and carrying the distance each
    // commit was weighed at, which is decorated onto its line below.
    let mut bisect_dist: HashMap<ObjectId, i64> = HashMap::new();
    // The six `bisect_*` assignments, rendered once the search is done and
    // written in place of (or, under `--bisect-all`, after) the listing.
    let mut bisect_vars_text: Option<String> = None;
    if bisect {
        let found = find_bisection(&commits, &parents_of, first_parent, bisect_all, &treesame);
        commits = found.commits.iter().map(|(id, _)| *id).collect();
        if bisect_all {
            bisect_dist = found.commits.iter().copied().collect();
        }
        if bisect_vars {
            // `show_bisect_vars()` bails out on an empty range before it prints
            // anything — `git rev-list --bisect-vars main ^main` writes nothing and
            // exits 1, which is the one exit-1 rev-list has that is not an error.
            // `--bisect-all` does not exempt it: stock answers the same way for
            // `--bisect-all --bisect-vars` over an empty range.
            if commits.is_empty() {
                return Ok(ExitCode::from(1));
            }
            let (reaches, all) = (found.reaches, found.all);
            let cnt = std::cmp::max(all - reaches, reaches);
            let rev = commits.first().map(ToString::to_string).unwrap_or_default();
            bisect_vars_text = Some(format!(
                "bisect_rev='{rev}'\nbisect_nr={}\nbisect_good={}\nbisect_bad={}\nbisect_all={all}\nbisect_steps={}\n",
                cnt - 1,
                all - reaches - 1,
                reaches - 1,
                estimate_bisect_steps(all),
            ));
        }
    }
    // `revs.show_decorations = 1` under `--bisect-all`: the listing carries the
    // ordinary short ref decorations, with the search's `dist=<n>` last.
    //
    // `cmd_rev_list()` loads them with a *NULL* filter, not `git log`'s
    // `set_default_decoration_filter()` one — so a ref outside the decorated
    // namespaces (`refs/top`) shows here even though `git log --decorate` hides
    // it. Hence `use_default = false` rather than log's `true`.
    let bisect_decorations = if bisect_all {
        let filter = super::log::DecorationFilter::build(&repo, &[], &[], false);
        Some(super::log::build_decorations(&repo, &filter)?)
    } else {
        None
    };

    // `set_children` runs over the limited list, before any output-time filter,
    // and prepends each child, so a commit's children come out newest first.
    let mut children_of: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    if show_children {
        for id in &commits {
            for parent in parents_of.get(id).into_iter().flatten() {
                children_of.entry(*parent).or_default().insert(0, *id);
            }
        }
    }

    // `--simplify-by-decoration`: the same simplification the pathspec path runs,
    // asking a different question of each commit. A commit that carries a decoration
    // is kept, and so are a root and a merge, because simplification may not change
    // the shape of the history; everything else is walked past, and what is then
    // unreachable from the tips drops out with it.
    if simplify_by_decoration {
        let filter = super::log::DecorationFilter::build(&repo, &[], &[], true);
        let decos = super::log::build_decorations(&repo, &filter)?;
        let kept: HashSet<ObjectId> = commits
            .iter()
            .copied()
            .filter(|id| {
                let parents = parents_of.get(id).map_or(0, Vec::len);
                decos.decorates(id) || parents == 0 || parents > 1
            })
            .collect();
        let mut reachable: HashSet<ObjectId> = HashSet::with_capacity(commits.len());
        let mut stack: Vec<ObjectId> = tips.clone();
        while let Some(id) = stack.pop() {
            if !reachable.insert(id) {
                continue;
            }
            let parents = parents_of.get(&id).map_or(&[][..], Vec::as_slice);
            if kept.contains(&id) {
                stack.extend(parents.iter().copied());
            } else {
                // A simplified-away commit is walked past along its first parent only.
                stack.extend(parents.first().copied());
            }
        }
        commits.retain(|id| kept.contains(id) && reachable.contains(id));
    }

    // `simplify_commit` drops the TREESAME commits, then `commit_ignore` applies
    // the parent-count bounds and `commit_match` the header predicates.
    commits.retain(|id| !treesame.contains(id));
    commits.retain(|id| {
        let n = parents_of.get(id).map_or(0, Vec::len);
        n as i64 >= min_parents && max_parents.is_none_or(|max| n <= max)
    });

    // `--grep`/`--author`/`--committer`: git's `commit_match`, applied as each
    // commit is about to be shown rather than during the walk.
    let cfilter = CommitFilter {
        // `rev-list` loads no mailmap, so its header greps see the recorded identities.
        ident_map: None,
        author_res: compile_patterns(
            &author_pats,
            dialect,
            ignore_case,
            crate::revfilter::Origin::Header,
        )?,
        committer_res: compile_patterns(
            &committer_pats,
            dialect,
            ignore_case,
            crate::revfilter::Origin::Header,
        )?,
        grep_res: compile_patterns(
            &grep_pats,
            dialect,
            ignore_case,
            crate::revfilter::Origin::CommandLine,
        )?,
        all_match,
        invert_grep,
    };
    if !cfilter.is_empty() {
        let mut kept = Vec::with_capacity(commits.len());
        for id in &commits {
            let object = repo.find_object(*id)?;
            if cfilter.matches(&object.into_commit())? {
                kept.push(*id);
            }
        }
        commits = kept;
    }

    // `simplify_commit` rewrites the parent list of every shown commit when
    // `--parents` asked for ancestry, so a parent the path limit simplified away
    // is reported as the nearest ancestor that survived. The call sits under
    // `revs->prune && revs->dense && want_ancestry(revs)` (revision.c:4317-4318),
    // so `--sparse --parents` prints the ancestry the commits really have — as
    // the in-place prune left it, not as `rewrite_parents()` would.
    if !pathspecs.is_empty() && show_parents && dense {
        // `--simplify-merges` already ran the same `rewrite_parents()` over the
        // list `simplify_one()` produced; rerunning it over the survivors would
        // rewrite a rewrite.
        let rewritten: Vec<(ObjectId, Vec<ObjectId>)> = if simplify_merges_opt {
            simplified_display.into_iter().collect()
        } else {
            let survivors: HashSet<ObjectId> = commits.iter().copied().collect();
            commits
                .iter()
                .map(|id| {
                    (
                        *id,
                        rewrite_parents(id, &survivors, &parents_of, first_parent),
                    )
                })
                .collect()
        };
        parents_of.extend(rewritten);
    }

    // `revs->skip_count` is spent inside `get_revision()` before the `max_count`
    // check, so `--skip=2 --max-count=1` answers the third commit rather than none.
    if skip_count > 0 {
        commits.drain(..skip_count.min(commits.len()));
    }
    if let Some(max) = max_count {
        // `cmd_rev_list()` stops calling `get_revision()` once the cap is spent, so
        // a walk whose remaining commits were all going to be dropped by the cap
        // never reaches the parent it cannot read: `git rev-list -n 1` over a
        // history whose second commit is grafted to a missing parent exits 0.
        if commits.len() >= max {
            abort = None;
        }
        commits.truncate(max);
    }

    // `--boundary`: the parents of the commits the walk returned that were not
    // themselves returned, appended once `get_revision_1()` runs dry. The marking
    // runs over the emission order, so it has to happen before `--reverse` — which
    // `get_revision()` applies to the *whole* sequence, boundary commits included
    // (revision.c:4673-4692), putting them in front and reversing their own order.
    let mut boundary_commits = if boundary {
        boundary_list(&repo, &commits, &mut parents_of, order == Order::DateTopo)
    } else {
        Vec::new()
    };

    // 6. Render.
    //
    // `--header` alone leaves `commit_format` unspecified, which rev-list then
    // sets to `raw` while `hdr_termination` stays NUL; an explicit `--pretty`
    // sets it to a newline and puts `commit ` in front of the object name.
    // Only a user format may drop the object-name line, so `--no-commit-header`
    // is ignored for every other format — including the `raw` that a bare
    // `--header` selects, which is still unspecified at the point git checks.
    include_header = include_header || !matches!(pretty, Some(Pretty::User(_)));
    let (pretty, hdr_term, header_prefix): (Option<Pretty>, u8, &[u8]) = match pretty {
        Some(p) => {
            let prefix: &[u8] = if matches!(p, Pretty::Oneline) || !include_header {
                b""
            } else {
                b"commit "
            };
            (Some(p), b'\n', prefix)
        }
        None if verbose_header => (Some(Pretty::Raw), 0, b""),
        None => (None, b'\n', b""),
    };

    let mut out: Vec<u8> = Vec::new();
    // `mark_edges_uninteresting()` (list-objects.c:283-321) runs before
    // `traverse_commit_list()`, so every `-<id>` precedes the listing itself. The
    // edges are the *uninteresting* parents of the shown commits — a parent left
    // unwalked by `--max-count` is not one, which is why `--objects-edge -n 1`
    // prints no edge at all.
    if edge_hint && !hidden.is_empty() {
        let uninteresting = reachable_from(&hidden, &parents_of);
        let shown: HashSet<ObjectId> = commits.iter().copied().collect();
        let mut edges: Vec<ObjectId> = Vec::new();
        let mut seen_edge: HashSet<ObjectId> = HashSet::new();
        for id in &commits {
            for parent in parents_of.get(id).into_iter().flatten() {
                if !shown.contains(parent)
                    && uninteresting.contains(parent)
                    && seen_edge.insert(*parent)
                {
                    edges.push(*parent);
                }
            }
        }
        for id in &edges {
            out.extend_from_slice(format!("-{id}\n").as_bytes());
        }
        // `parent->object.flags |= SHOWN` right before `show_edge(parent)`
        // (list-objects.c), and `create_boundary_commit_list()` skips a commit
        // that carries `SHOWN` (revision.c) — so `--objects-edge --boundary`
        // prints the shared commit once, here, rather than twice.
        boundary_commits.retain(|id| !edges.contains(id));
    }
    let mut count_left = 0usize;
    let mut count_right = 0usize;
    let mut count_same = 0usize;
    let mut disk_total: u64 = 0;

    // Objects reachable from an excluded (`^rev`) commit are pre-marked as seen
    // so they never appear, which is how git keeps `a..b --objects` to b's data.
    let object_specs = if pathspecs.is_empty() {
        None
    } else {
        Some(super::log::PathspecMatcher::new(&repo, &pathspecs)?)
    };
    let walk = ObjectWalk {
        filter,
        missing,
        collect_omits: print_omitted,
        pathspecs: object_specs.as_ref(),
    };
    let mut absent: Vec<MissingObject> = Vec::new();
    // `--filter-print-omitted`'s `omitted_objects` set, filled by the filter and
    // printed once the walk is over.
    let mut omitted: Vec<ObjectId> = Vec::new();
    // The same UNINTERESTING marking reaches the object walk: a tree or blob a
    // promisor pack holds is already "seen" and is never listed.
    let mut seen: HashSet<ObjectId> = match exclude_promisor {
        true => promisor_pack_objects(&repo),
        false => HashSet::new(),
    };
    // `mark_edges_uninteresting()` (`list-objects.c:283-321`) walks `revs->commits`
    // — the list `prepare_revision_walk()` left behind — and marks the trees of
    // the uninteresting commits *in it*. An exclusion that leaves nothing to walk
    // leaves that list empty, so nothing is marked and a tree named on the
    // command line survives: stock `git rev-list --objects ^main main` prints
    // nothing while `git rev-list --objects ^main main^{tree}` prints the whole
    // tree.
    if objects && !hidden.is_empty() && !commits.is_empty() {
        mark_hidden_objects(&repo, &hidden, first_parent, &mut seen)?;
    }
    // Each entry is the object and the path it was reached through, which
    // `--no-object-names` drops at render time. The tag objects seeded from refs
    // come ahead of any tree, named by the tag's own name field.
    let mut object_lines: Vec<(ObjectId, Vec<u8>)> = Vec::new();
    if objects {
        // `traverse_non_commits()` (`list-objects.c:344-375`), in pending order:
        // a tag prints its own line and stops there, a blob prints one line, and
        // a tree prints its own line and then everything under it — with
        // `pending->path` as the base, so `git rev-list --objects main:sub`
        // names the entries `sub/…`.
        // `mark_tree_contents_uninteresting()` runs while the pending list is
        // still being built, so it is a pass of its own — an excluded tree hides
        // its contents from an interesting tree named *before* it, not only after.
        for entry in pending.iter().filter(|e| e.uninteresting) {
            match entry.kind {
                gix::object::Kind::Tree => mark_tree_seen(&repo, entry.id, &mut seen),
                _ => {
                    seen.insert(entry.id);
                }
            }
        }
        for entry in pending.iter().filter(|e| !e.uninteresting) {
            if !seen.insert(entry.id) {
                continue;
            }
            match entry.kind {
                gix::object::Kind::Tree => {
                    object_lines.push((entry.id, entry.name.clone()));
                    if let Err(code) = walk_tree(
                        &repo,
                        entry.id,
                        &entry.name,
                        0,
                        &mut seen,
                        &mut object_lines,
                        &mut absent,
                        &mut omitted,
                        &walk,
                    )? {
                        return Ok(code);
                    }
                }
                gix::object::Kind::Blob => match blob_filtered(&repo, entry.id, &entry.name, &mut absent, &walk)?
                {
                    Ok(BlobVerdict::Filtered) => omitted.push(entry.id),
                    Ok(BlobVerdict::Absent) => {}
                    Ok(BlobVerdict::Show) => object_lines.push((entry.id, entry.name.clone())),
                    Err(code) => return Ok(code),
                },
                _ => object_lines.push((entry.id, entry.name.clone())),
            }
        }
    }
    let object_line = |id: &ObjectId, name: &[u8], out: &mut Vec<u8>| {
        out.extend_from_slice(id.to_string().as_bytes());
        if object_names {
            out.push(b' ');
            out.extend_from_slice(name);
        }
        out.push(b'\n');
    };

    // Shown commits first, then the boundary commits git appends once the walk
    // has run out; both go through the same renderer. `--reverse` collects that
    // whole sequence through `commit_list_insert` and pops it back
    // (revision.c:4673-4692), so it reverses the two halves together.
    let mut emitted: Vec<(ObjectId, bool)> = commits
        .iter()
        .map(|id| (*id, false))
        .chain(boundary_commits.iter().map(|id| (*id, true)))
        .collect();
    if reverse {
        emitted.reverse();
        // The `--objects` walk below reads the commit list itself, in the same
        // order the records came out.
        commits.reverse();
    }

    // `--graph`: one block per record, drawn behind the graph rows once the whole
    // list is known. `show_log()` renders the record and hands it to
    // `graph_show_commit_msg()`, which is what [`super::log::render_graph`] does
    // with these.
    let mut graph_blocks: Vec<Option<super::log::GraphBlock>> = Vec::new();

    for (id, is_boundary) in &emitted {
        let record_start = out.len();
        if disk_usage {
            match object_disk_size(&repo, *id) {
                Some(n) => disk_total += n,
                None => return Ok(fatal(&format!("unable to get disk usage of {id}"))),
            }
        }
        if count_only && !quiet {
            // `--count` with `--cherry-mark` reports the equivalent commits in a column of their
            // own rather than among the two sides (`print_commit_counts()`), which is how
            // `3\t2` distinguishes "three commits, two of them already upstream".
            if cherry_mark && patch_same.contains(id) {
                count_same += 1;
            } else if left.contains(id) {
                count_left += 1;
            } else {
                count_right += 1;
            }
        }
        // `show_commit` returns before rendering under `--quiet` and `--count`,
        // but `traverse_commit_list` still visits the objects behind each commit,
        // so the interleaved `--in-commit-order` listing below is not skipped.
        if !quiet && !count_only {
            out.extend_from_slice(header_prefix);
            // `--timestamp`: `show_commit()` prints the commit date in front of the
            // object name, which is how a caller sorts a list without re-reading each
            // object.
            if show_timestamp {
                let secs = repo
                    .find_object(*id)
                    .ok()
                    .and_then(|o| o.try_into_commit().ok())
                    .and_then(|c| c.committer().ok().map(|s| s.seconds()))
                    .unwrap_or(0);
                out.extend_from_slice(secs.to_string().as_bytes());
                out.push(b' ');
            }
            if include_header {
                // `get_revision_mark()` is the graph's own glyph under `--graph`:
                // `graph_show_commit()` draws `<`, `>`, `=` and `o` in the column
                // where it would otherwise draw `*`, so the mark is not also
                // printed in front of the object name.
                if !graph {
                    out.extend_from_slice(revision_mark(
                        *is_boundary,
                        left.contains(id),
                        patch_same.contains(id),
                        left_right,
                        cherry_mark,
                    ));
                }
                // `if (revs->abbrev_commit && revs->abbrev)` — both are needed,
                // which is why `--abbrev=8` alone prints the whole id and
                // `--abbrev-commit --no-abbrev` does too.
                if abbrev_commit && abbrev_len != Some(0) {
                    out.extend_from_slice(id.attach(&repo).shorten_or_id().to_string().as_bytes());
                } else {
                    out.extend_from_slice(id.to_string().as_bytes());
                }
            }
            // `--bisect-all`'s decoration list: the refs pointing at the commit,
            // then the `dist=<n>` `best_bisection_sorted()` attached to it, in
            // `format_decorations()`'s ` (a, b)` shape.
            if let Some(decos) = &bisect_decorations {
                let mut refs: Vec<u8> = Vec::new();
                super::log::format_decorations(
                    &mut refs,
                    decos,
                    id,
                    false,
                    &super::color::DecorateColors::disabled(),
                    &super::log::DecorationOpts { prefix: "", suffix: "", ..Default::default() },
                );
                out.extend_from_slice(b" (");
                if !refs.is_empty() {
                    out.extend_from_slice(&refs);
                    out.extend_from_slice(b", ");
                }
                let dist = bisect_dist.get(id).copied().unwrap_or(0);
                out.extend_from_slice(format!("dist={dist})").as_bytes());
            }
            if show_parents {
                for parent in parents_of.get(id).into_iter().flatten() {
                    out.push(b' ');
                    out.extend_from_slice(parent.to_string().as_bytes());
                }
            }
            if show_children {
                for child in children_of.get(id).into_iter().flatten() {
                    out.push(b' ');
                    out.extend_from_slice(child.to_string().as_bytes());
                }
            }
            match &pretty {
                // git separates the object name from a oneline body with a space and
                // from every other body with the line terminator.
                Some(Pretty::Oneline) => out.push(b' '),
                _ if include_header => out.push(b'\n'),
                _ => {}
            }
            if let Some(p) = &pretty {
                let object = repo.find_object(*id)?;
                let body = rev_list_pretty_body(&repo, &object.into_commit(), p, &date_mode)?;
                if !body.is_empty() {
                    out.extend_from_slice(&body);
                    out.push(hdr_term);
                }
            }
        }
        if graph {
            // A commit that printed nothing is `None`: git still runs it through
            // `graph_update()`, so its lane moves on with no row of its own.
            let text = out.split_off(record_start);
            graph_blocks.push((!text.is_empty()).then(|| super::log::GraphBlock::message_only(text)));
        }
        if objects && in_commit_order && !is_boundary {
            if let Err(code) = collect_commit_objects(
                &repo,
                *id,
                &mut seen,
                &mut object_lines,
                &mut absent,
                &mut omitted,
                &walk,
            )? {
                return Ok(code);
            }
            for (oid, name) in object_lines.drain(..) {
                if disk_usage {
                    match object_disk_size(&repo, oid) {
                        Some(n) => disk_total += n,
                        None => return Ok(fatal(&format!("unable to get disk usage of {oid}"))),
                    }
                }
                count_right += 1;
                if !quiet && !count_only {
                    object_line(&oid, &name, &mut out);
                }
            }
        }
    }

    if graph {
        // The nodes the graph state machine walks, in emission order.
        // `rev-list` has no `--color` option, so `revs->diffopt.use_color` is never
        // turned on and the graph is drawn plain.
        let shown: HashSet<ObjectId> =
            emitted.iter().filter(|(_, b)| !*b).map(|(id, _)| *id).collect();
        // `--boundary`: every parent of a commit the walk *returned* carries
        // CHILD_SHOWN, which `graph_is_interesting()` accepts on its own; the
        // boundary commits themselves are handed out below the marking loop and
        // mark none of theirs.
        let child_shown: HashSet<ObjectId> = match boundary {
            true => emitted
                .iter()
                .filter(|(_, b)| !*b)
                .flat_map(|(id, _)| parents_of.get(id).into_iter().flatten().copied())
                .collect(),
            false => HashSet::new(),
        };
        let nodes: Vec<super::log::Node> = emitted
            .iter()
            .map(|(id, is_boundary)| super::log::Node {
                id: *id,
                parents: parents_of.get(id).cloned().unwrap_or_default(),
                time: 0,
                source: String::new(),
                seq: 0,
                boundary: *is_boundary,
                symmetric_left: left.contains(id),
                patch_same: patch_same.contains(id),
                follow_path: None,
                graph_width: 0,
                reflog: None,
            })
            .collect();
        let interest = super::log::GraphInterest { child_shown, shown };
        // Every record here already ends in its own terminator, which
        // `show_log()` prints after the commit's remaining graph rows.
        let drawn = super::log::render_graph(
            &nodes,
            &graph_blocks,
            super::log::graph_colors(&repo),
            false,
            None,
            Some(hdr_term),
            first_parent,
            left_right,
            &interest,
        )?;
        out.extend_from_slice(&drawn);
    }

    // `traverse_commit_list()` (list-objects.c) drains `get_revision()` first and
    // only then calls `traverse_non_commits()`, so a walk that died never reaches
    // the object listing at all. `--in-commit-order` is the exception: it emits a
    // commit's objects inside the loop, alongside the commit, and those stand.
    if objects && !in_commit_order && abort.is_none() {
        for id in &commits {
            if let Err(code) = collect_commit_objects(
                &repo,
                *id,
                &mut seen,
                &mut object_lines,
                &mut absent,
                &mut omitted,
                &walk,
            )? {
                return Ok(code);
            }
        }
        for (id, name) in &object_lines {
            if disk_usage {
                match object_disk_size(&repo, *id) {
                    Some(n) => disk_total += n,
                    None => return Ok(fatal(&format!("unable to get disk usage of {id}"))),
                }
            }
            // Objects are always counted on the right, because a marked count and
            // `--objects` are mutually exclusive.
            count_right += 1;
            if !quiet && !count_only {
                object_line(id, name, &mut out);
            }
        }
    }

    let stdout = std::io::stdout();
    let mut sink = stdout.lock();
    // Everything below the walk is a *summary* rather than a stream: a count, a
    // disk-usage total, a `--reverse`d listing. git computes those inside the
    // `while ((commit = get_revision(revs)))` loop and prints them once it ends,
    // so a walk that died never reaches the print — only the plain listing, which
    // git emits commit by commit, keeps the prefix it managed to produce.
    if let Some(abort) = abort {
        // A pathspec puts `try_to_simplify_commit()` (revision.c:1182) ahead of
        // `process_parents()`'s parent loop, and its tree diff hits the unreadable
        // parent first — see [`super::log::WalkAbort::die_simplify`].
        let die = match pathspecs.is_empty() {
            true => super::log::WalkAbort::die_traverse,
            false => super::log::WalkAbort::die_simplify,
        };
        if count_only || quiet || disk_usage || reverse {
            return Ok(die(abort));
        }
        sink.write_all(&out)?;
        sink.flush()?;
        return Ok(die(abort));
    }
    // `show_bisect_vars()` prints the listing only under `BISECT_SHOW_ALL`, with a
    // `------` rule between it and the variables, and then `goto cleanup` — so the
    // `--count`, `--disk-usage` and omitted-object summaries never run.
    if let Some(vars) = bisect_vars_text {
        if bisect_all {
            sink.write_all(&out)?;
            writeln!(sink, "------")?;
        }
        sink.write_all(vars.as_bytes())?;
        sink.flush()?;
        return Ok(ExitCode::SUCCESS);
    }
    if count_only {
        if left_right && cherry_mark {
            writeln!(sink, "{count_left}\t{count_right}\t{count_same}")?;
        } else if left_right {
            writeln!(sink, "{count_left}\t{count_right}")?;
        } else if cherry_mark {
            writeln!(sink, "{}\t{count_same}", count_left + count_right)?;
        } else {
            writeln!(sink, "{}", count_left + count_right)?;
        }
    } else {
        sink.write_all(&out)?;
    }
    // ```c
    // if (arg_print_omitted) {
    //         oidset_iter_init(&omitted_objects, &iter);
    //         while ((oid = oidset_iter_next(&iter)))
    //                 printf("~%s\n", oid_to_hex(oid));
    // }
    // if (arg_missing_action == MA_PRINT || arg_missing_action == MA_PRINT_INFO) {
    //         oidmap_iter_init(&missing_objects, &iter);
    //         while ((entry = oidmap_iter_next(&iter)))
    //                 print_missing_object(entry, …);
    // }
    // ```
    //
    // (`builtin/rev-list.c:989-1010`.) Both are hash tables rather than lists, so
    // neither comes out in the order the walk found them — and the two use
    // different tables, so the same ids order differently in the two blocks. See
    // [`crate::oidhash`].
    if print_omitted {
        for id in crate::oidhash::khash_order(&omitted) {
            writeln!(sink, "~{id}")?;
        }
    }
    // The map is keyed by id, so the order comes from the ids alone; the entry
    // each one names supplies the two fields `print-info` appends.
    let absent_ids: Vec<ObjectId> = absent.iter().map(|entry| entry.id).collect();
    for id in crate::oidhash::hashmap_order(&absent_ids) {
        let Some(entry) = absent.iter().find(|entry| entry.id == id) else {
            continue;
        };
        sink.write_all(&print_missing_object(entry, missing == Missing::PrintInfo))?;
    }
    if disk_usage {
        if disk_usage_human {
            writeln!(sink, "{}", human_size(disk_total))?;
        } else {
            writeln!(sink, "{disk_total}")?;
        }
    }
    sink.flush()?;

    Ok(ExitCode::SUCCESS)
}

/// A long option's value together with how many argv slots it took, so the caller
/// can advance past `--opt=value` (one) or `--opt value` (two) alike.
struct LongOpt {
    value: String,
    consumed: usize,
}

/// git's `parse_long_opt`: match `--<name>=<value>` or `--<name> <value>`.
fn long_opt_value(args: &[String], i: usize, name: &str) -> Option<LongOpt> {
    let a = args[i].as_str();
    let bare = format!("--{name}");
    if let Some(v) = a.strip_prefix(&format!("{bare}=")) {
        return Some(LongOpt {
            value: v.to_string(),
            consumed: 1,
        });
    }
    if a == bare {
        return Some(LongOpt {
            value: args.get(i + 1).cloned().unwrap_or_default(),
            consumed: 2,
        });
    }
    None
}

/// `cherry_pick_list()` (revision.c) — mark the commits whose *change* appears on both sides of a
/// symmetric difference.
///
/// ```c
/// if (!left_count || !right_count)
///         return 0;
/// left_first = left_count < right_count;
/// … /* patch ids for the smaller side */
/// … /* look the larger side up in that table */
/// ```
///
/// The smaller side is the one that gets a patch-id table, because that is the side whose diffs
/// have to be held in memory; every commit on the other side is then a lookup. A merge has no
/// patch id (`commit_patch_id()` answers `None`) and can never be equivalent.
///
/// Returns the ids marked `PATCHSAME` — on *both* sides, since git flags the pair.
fn cherry_pick_list(
    repo: &gix::Repository,
    commits: &[ObjectId],
    left: &HashSet<ObjectId>,
) -> Result<HashSet<ObjectId>> {
    let (mut lefts, mut rights): (Vec<ObjectId>, Vec<ObjectId>) = (Vec::new(), Vec::new());
    for id in commits {
        match left.contains(id) {
            true => lefts.push(*id),
            false => rights.push(*id),
        }
    }
    let mut same = HashSet::new();
    if lefts.is_empty() || rights.is_empty() {
        return Ok(same);
    }
    // `left_first = left_count < right_count`.
    let (table_side, probe_side) = match lefts.len() < rights.len() {
        true => (&lefts, &rights),
        false => (&rights, &lefts),
    };

    let mut ids: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    for id in table_side {
        if let Some(pid) = super::cherry::commit_patch_id(repo, *id)? {
            ids.entry(pid).or_default().push(*id);
        }
    }
    for id in probe_side {
        let Some(pid) = super::cherry::commit_patch_id(repo, *id)? else {
            continue;
        };
        // `patch_id_iter_first()`: one match is enough, and git flags the commit it found as
        // well as the one it was looking for.
        if let Some(matches) = ids.get(&pid) {
            if let Some(other) = matches.first() {
                same.insert(*id);
                same.insert(*other);
            }
        }
    }
    Ok(same)
}

/// git's `get_revision_mark`: the character printed in front of the object name.
///
/// A boundary commit wins over everything, then a patch-equivalent one (which
/// this port never produces, see `--cherry-mark`), then the symmetric side under
/// `--left-right`, and finally `--cherry-mark`'s plain `+`.
fn revision_mark(
    is_boundary: bool,
    is_left: bool,
    is_patch_same: bool,
    left_right: bool,
    cherry_mark: bool,
) -> &'static [u8] {
    if is_boundary {
        b"-"
    } else if is_patch_same {
        // `else if (commit->object.flags & PATCHSAME) return "=";` — ahead of the symmetric
        // side, so `--cherry-mark --left-right` prints `=` rather than `<`/`>` for a commit that
        // exists on both sides.
        b"="
    } else if left_right {
        if is_left {
            b"<"
        } else {
            b">"
        }
    } else if cherry_mark {
        b"+"
    } else {
        b""
    }
}

/// Seed from a ref set: `--all`, `--branches`, `--tags`, `--remotes` (each with
/// an optional `=<pattern>`) and `--glob=<pattern>`, minus whatever `--exclude`
/// patterns the selection consumed.
///
/// `--all` is every ref under `refs/` in name order followed by `HEAD`; the
/// others are their own namespace only, with no `HEAD`. Which refs a selection
/// yields is [`super::log::RefSelection`]'s business — it is the same
/// `refs_for_each_ref_ext()` rule `git log` uses.
/// `exclude_hidden_refs()` (`revision.c`) → `parse_hide_refs_config()`
/// (`refs.c:1688-1708`): the `transfer.hideRefs` and `<section>.hideRefs`
/// patterns `--exclude-hidden=<section>` installs.
///
/// ```c
/// if (strcmp(section, "fetch") && strcmp(section, "receive") &&
///                 strcmp(section, "uploadpack"))
///         die(_("unsupported section for hidden refs: %s"), section);
/// ```
///
/// Trailing slashes are stripped from each pattern, because
/// [`ref_is_hidden`] matches on a `/` boundary of its own.
///
/// Known gap: git reads the two keys through one `repo_config()` pass, so their
/// relative order is the order they appear in the config *files*; this reads each
/// key separately and puts `transfer.hideRefs` first. The order is observable only
/// through a `!`-negated pattern that overlaps a positive one from the other key.
pub(super) fn hidden_ref_patterns(repo: &gix::Repository, section: &str) -> Result<Vec<String>, String> {
    if !matches!(section, "fetch" | "receive" | "uploadpack") {
        return Err(format!("unsupported section for hidden refs: {section}"));
    }
    let snapshot = repo.config_snapshot();
    let mut patterns = Vec::new();
    for key in ["transfer.hideRefs", &format!("{section}.hideRefs")] {
        for value in snapshot.strings(key).into_iter().flatten() {
            let mut pattern = value.to_string();
            while pattern.ends_with('/') {
                pattern.pop();
            }
            patterns.push(pattern);
        }
    }
    Ok(patterns)
}

/// `ref_is_hidden()` (`refs.c:1710-1740`): the *last* matching pattern decides,
/// `!` negates it, and a match is a prefix that ends at the end of the name or at
/// a `/`. `^` selects the un-stripped name, which is the same string here because
/// this port has no ref namespaces.
pub(super) fn ref_is_hidden(refname: &str, patterns: &[String]) -> bool {
    for pattern in patterns.iter().rev() {
        let (negated, pattern) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, pattern.as_str()),
        };
        let pattern = pattern.strip_prefix('^').unwrap_or(pattern);
        if let Some(rest) = refname.strip_prefix(pattern) {
            if rest.is_empty() || rest.starts_with('/') {
                return !negated;
            }
        }
    }
    false
}

/// `add_index_objects_to_pending()` → `do_add_index_objects_to_pending()`
/// (`revision.c:1797-1827`): every index blob, then every valid cache-tree.
///
/// ```c
/// for (i = 0; i < istate->cache_nr; i++) {
///         struct cache_entry *ce = istate->cache[i];
///         if (S_ISGITLINK(ce->ce_mode))
///                 continue;
///         …
///         add_pending_object_with_path(revs, &blob->object, "", ce->ce_mode, ce->name);
/// }
/// if (istate->cache_tree) { … add_cache_tree(istate->cache_tree, revs, &path, flags); }
/// ```
///
/// The blob's pending *path* is the index path and its *name* is empty, so
/// `git rev-list --objects --indexed-objects` labels each blob with its path.
/// `add_cache_tree()` pends the root under the empty path and each subtree under
/// its directory path, and only when `entry_count >= 0` — an invalidated
/// cache-tree entry contributes nothing.
///
/// `revs->single_worktree` is not consulted: this port reads the current index
/// only, so the linked-worktree indexes git also scans are not represented.
fn seed_index_objects(
    repo: &gix::Repository,
    negate: bool,
    pending: &mut Vec<Pending>,
) -> Result<(), String> {
    let index = repo.index_or_empty().map_err(|e| e.to_string())?;
    for entry in index.entries() {
        if entry.mode.is_submodule() {
            continue;
        }
        pending.push(Pending {
            id: entry.id,
            name: entry.path(&index).to_vec(),
            kind: gix::object::Kind::Blob,
            uninteresting: negate,
        });
    }
    if let Some(tree) = index.tree() {
        add_cache_tree(tree, Vec::new(), negate, pending);
    }
    Ok(())
}

/// `add_cache_tree()` (`revision.c:1742-1762`), depth-first from the root.
fn add_cache_tree(
    tree: &gix::index::extension::Tree,
    path: Vec<u8>,
    negate: bool,
    pending: &mut Vec<Pending>,
) {
    // `if (it->entry_count >= 0)`: a subtree whose entry count was invalidated by
    // an index change is skipped, though its children are still visited.
    if tree.num_entries.is_some() {
        pending.push(Pending {
            id: tree.id,
            name: path.clone(),
            kind: gix::object::Kind::Tree,
            uninteresting: negate,
        });
    }
    for child in &tree.children {
        let mut child_path = path.clone();
        if !child_path.is_empty() {
            child_path.push(b'/');
        }
        child_path.extend_from_slice(&child.name);
        add_cache_tree(child, child_path, negate, pending);
    }
}

fn seed_ref_set(
    repo: &gix::Repository,
    sel: &super::log::RefSelection,
    negate: bool,
    hidden: &[String],
    seeds: &mut Vec<Seed>,
    pending: &mut Vec<Pending>,
) -> Result<(), String> {
    let refs = repo.references().map_err(|e| e.to_string())?;
    let iter = refs.all().map_err(|e| e.to_string())?;
    for reference in iter {
        let reference = reference.map_err(|e| e.to_string())?;
        let full = reference.name().as_bstr().to_string();
        let Some(name) = sel.selects(&full) else { continue };
        // `ref_excluded()` tests the `--exclude` patterns and then
        // `ref_is_hidden()`, both against the name `handle_one_ref()` was given.
        if ref_is_hidden(name, hidden) {
            continue;
        }
        let target = match reference.try_id() {
            Some(id) => id.detach(),
            // Symbolic: follow it, but then there is no tag object to record.
            None => match reference.into_fully_peeled_id() {
                Ok(id) => id.detach(),
                Err(_) => continue,
            },
        };
        // `handle_one_ref()` hands every selected ref to `get_reference()`, which
        // `die(_("bad object %s"), path)`s when `parse_object()` cannot read the
        // id (revision.c:389-400). `path` is the name the iterator reported — the
        // full ref for `--all`, the trimmed one for `--branches`/`--tags`/
        // `--remotes`, which is `sel.selects()`'s answer here.
        if repo.find_object(target).is_err() {
            return Err(format!("fatal: bad object {name}\n"));
        }
        if let Some(id) = peel_recording_tags(repo, target, pending) {
            seeds.push(Seed {
                id,
                uninteresting: negate,
                symmetric_left: false,
                bottom: negate,
            });
        }
    }
    // `handle_refs(refs, revs, flags, refs_head_ref)`: `--all` pends `HEAD` too,
    // after the ref list and under that literal name — which is why a `refs/…`
    // exclusion pattern never removes it.
    if sel.head && !sel.excluded("HEAD") && !ref_is_hidden("HEAD", hidden) {
        if let Ok(head) = repo.head_id() {
            if let Some(id) = peel_recording_tags(repo, head.detach(), pending) {
                seeds.push(Seed {
                    id,
                    uninteresting: negate,
                    symmetric_left: false,
                    bottom: negate,
                });
            }
        }
    }
    Ok(())
}

/// git's `handle_revision_arg`: turn one revision word into seeds.
///
/// A leading `^` flips the sense of the revision, `<a>..<b>` excludes everything
/// `<a>` reaches, and `<a>...<b>` excludes their merge bases and marks `<a>` as
/// the symmetric left side. `--not` has already flipped `negate`, and `^` flips
/// it once more, exactly as git XORs `UNINTERESTING | BOTTOM` in both places.
fn seed_revision(
    repo: &gix::Repository,
    spec: &str,
    negate: bool,
    cant_be_filename: bool,
    seeds: &mut Vec<Seed>,
    pending: &mut Vec<Pending>,
) -> Result<(), String> {
    let unknown = |s: &str| unresolvable_in(repo, s, cant_be_filename);
    // `handle_revision_arg_1()`'s parent-mark block, decoded once for every verb
    // by [`crate::objname::parents_only`]: `<rev>^@` pends the parents alone and
    // returns, while `<rev>^!` and `<rev>^-<n>` pend the selected parents with
    // `flags ^ (UNINTERESTING | BOTTOM)` and then put the truncated name back in
    // `arg` so the commit itself is pended after them.
    //
    // The mark is found with `strstr`'s first-match rule rather than by stripping
    // a suffix, which is why `main^!^!` carries no mark at all.
    let spec: &str = match crate::objname::parents_only(spec) {
        crate::objname::ParentsOnly::Absent => spec,
        // `strtol_i()` refused the `<n>`, so `add_parents_only()` is never
        // reached and the operand is neither resolved nor queued.
        crate::objname::ParentsOnly::BadParent => return Err(unknown(spec)),
        crate::objname::ParentsOnly::Mark { base, nth, replaces } => {
            // `^@` keeps `flags`; `^!` and `^-<n>` pass
            // `flags ^ (UNINTERESTING | BOTTOM)`.
            let sense = if replaces { negate } else { !negate };
            let mut queued = Vec::new();
            let mut queue = |_name: &str, id: ObjectId, not: bool| queued.push((id, not));
            let answer = crate::objname::add_parents_only(repo, base, sense, nth, &mut queue);
            match answer {
                // `get_reference()`'s `die(_("bad object %s"), name)`, naming the
                // base with its leading `^` already stripped.
                crate::objname::Parents::BadObject => {
                    let name = crate::objname::uninteresting_mark(base).0;
                    return Err(format!("bad object {name}"));
                }
                // `return 0` leaves `arg` alone, so the operand carries its mark
                // into the ordinary resolution — where it cannot resolve.
                crate::objname::Parents::None => return Err(unknown(spec)),
                crate::objname::Parents::Queued => {}
            }
            for (id, not) in queued {
                seeds.push(Seed {
                    id,
                    uninteresting: not,
                    symmetric_left: false,
                    bottom: not,
                });
            }
            // `if (add_parents_only(…)) { ret = 0; goto out; }` — `^@` claimed the
            // operand outright and never pends the named commit.
            if replaces {
                return Ok(());
            }
            // `arg = arg_minus_excl;` / `arg = arg_minus_dash;` — the base still
            // carries its own leading `^`, which the exclusion step below strips
            // for a *second* time, exactly as `handle_revision_arg_1()` does.
            base
        }
    };

    // `if (*arg == '^') { local_flags = UNINTERESTING | BOTTOM; arg++; }`, which
    // `handle_revision_arg_1()` reaches only after the marks are done with.
    // `verify_non_filename()` (setup.c:281-291) as `handle_revision_arg_1()` runs
    // it: after the name resolved and before `get_reference()`. `rev-list` shares
    // `setup_revisions()` with `log`, so the message and the `cant_be_filename`
    // gate are the shared ones.
    let non_filename = |name: &str| -> Option<String> {
        if cant_be_filename {
            return None;
        }
        crate::setup::verify_non_filename(repo, name).map(|m| format!("fatal: {m}\n"))
    };
    if let Some(rest) = spec.strip_prefix('^') {
        let Some(id) = resolve(repo, rest, pending) else {
            // `handle_commit()`'s tree/blob arms again: an excluded non-commit
            // pends nothing and is not an error. Stock `git rev-list --objects
            // ^main^{tree}` exits 0 with no output.
            if pend_non_commit(repo, rest, !negate, pending) {
                return Ok(());
            }
            return Err(unknown(spec));
        };
        // The `^` is already consumed, so the name git checks is what follows it.
        if let Some(message) = non_filename(rest) {
            return Err(message);
        }
        seeds.push(Seed {
            id,
            uninteresting: !negate,
            symmetric_left: false,
            bottom: !negate,
        });
        return Ok(());
    }
    if let Some((l, r)) = spec.split_once("...") {
        let left_spec = if l.is_empty() { "HEAD" } else { l };
        let right_spec = if r.is_empty() { "HEAD" } else { r };
        let left = resolve(repo, left_spec, pending).ok_or_else(|| unknown(spec))?;
        let right = resolve(repo, right_spec, pending).ok_or_else(|| unknown(spec))?;
        // `handle_dotdot_1()` restores the separator first, so the token is
        // checked as written rather than endpoint by endpoint.
        if let Some(message) = non_filename(spec) {
            return Err(message);
        }
        let bases = repo
            .merge_bases_many(left, &[right])
            .map_err(|e| e.to_string())?;
        for base in bases {
            seeds.push(Seed {
                id: base.detach(),
                uninteresting: !negate,
                symmetric_left: false,
                bottom: !negate,
            });
        }
        seeds.push(Seed {
            id: left,
            uninteresting: negate,
            symmetric_left: true,
            bottom: negate,
        });
        seeds.push(Seed {
            id: right,
            uninteresting: negate,
            symmetric_left: false,
            bottom: negate,
        });
        return Ok(());
    }
    if let Some((l, r)) = spec.split_once("..") {
        let left_spec = if l.is_empty() { "HEAD" } else { l };
        let right_spec = if r.is_empty() { "HEAD" } else { r };
        let left = resolve(repo, left_spec, pending).ok_or_else(|| unknown(spec))?;
        let right = resolve(repo, right_spec, pending).ok_or_else(|| unknown(spec))?;
        // Same restore-then-check as the symmetric form above.
        if let Some(message) = non_filename(spec) {
            return Err(message);
        }
        seeds.push(Seed {
            id: left,
            uninteresting: !negate,
            symmetric_left: false,
            bottom: !negate,
        });
        seeds.push(Seed {
            id: right,
            uninteresting: negate,
            symmetric_left: false,
            bottom: negate,
        });
        return Ok(());
    }
    seed_plain(repo, spec, negate, cant_be_filename, seeds, pending)
}

/// Record the commits reading one revision word caused to be parsed.
///
/// Only `--no-walk` cares: nothing paints UNINTERESTING there, so how far
/// `mark_parents_uninteresting()` reaches is decided by which commits already
/// had their parent list loaded (see [`super::log::no_walk_uninteresting`]).
/// Two things load one: navigating a `~<n>`/`^<n>` chain, and the merge-base
/// search a `<a>...<b>` runs, which parses its way past the bases it finds.
fn note_parsed(
    repo: &gix::Repository,
    spec: &str,
    added: &[Seed],
    parsed: &mut HashSet<ObjectId>,
) -> Result<()> {
    for endpoint in spec.trim_start_matches('^').split("..") {
        let e = endpoint.trim_start_matches('.');
        parsed.extend(super::log::navigation_path(repo, if e.is_empty() { "HEAD" } else { e }));
    }
    if spec.contains("...") {
        let bases: Vec<ObjectId> = added.iter().map(|s| s.id).collect();
        parsed.extend(super::log::ancestor_closure(repo, &bases)?);
    }
    Ok(())
}

/// One ordinary revision word, pended with the flags `--not` left in force.
fn seed_plain(
    repo: &gix::Repository,
    spec: &str,
    negate: bool,
    cant_be_filename: bool,
    seeds: &mut Vec<Seed>,
    pending: &mut Vec<Pending>,
) -> Result<(), String> {
    let Some(id) = resolve(repo, spec, pending) else {
        // `get_oid_basic()` reads a ref without touching the object it names, so a
        // ref pointing at something the database does not have resolves and then
        // dies in `get_reference()`: `die(_("bad object %s"), name)`
        // (revision.c:389-400), naming the operand. That beats the unresolvable
        // text below, which is for a name `get_oid_basic()` itself refused.
        if let Some(named) = repo
            .rev_parse_single(crate::objname::canonical_spec(repo, spec).as_ref())
            .ok()
            .map(|id| id.detach())
        {
            if repo.find_object(named).is_err() {
                return Err(format!("fatal: bad object {spec}\n"));
            }
        }
        // `get_reference()` answers for any object type; only `handle_commit()`
        // insists on a commit, and its tree and blob arms pend rather than fail.
        // So `git rev-list main^{tree}` and `git rev-list main:base.txt` are not
        // errors at all — they simply contribute no commits.
        if pend_non_commit(repo, spec, negate, pending) {
            return Ok(());
        }
        return Err(unresolvable_in(repo, spec, cant_be_filename));
    };
    // `verify_non_filename()` (revision.c:2156-2157), between the name resolving
    // and `get_reference()`: an operand that is both a revision and a working-tree
    // path is ambiguous unless a `--` was seen.
    if !cant_be_filename {
        if let Some(message) = crate::setup::verify_non_filename(repo, spec) {
            return Err(format!("fatal: {message}\n"));
        }
    }
    seeds.push(Seed {
        id,
        uninteresting: negate,
        symmetric_left: false,
        bottom: negate,
    });
    Ok(())
}

/// `handle_commit()`'s tree and blob arms (`revision.c`), for an operand that
/// resolved to something [`resolve`] could not peel to a commit.
///
/// ```c
/// if (object->type == OBJ_TREE) {
///         struct tree *tree = (struct tree *)object;
///         if (!revs->tree_objects)
///                 return NULL;
///         if (flags & UNINTERESTING) {
///                 mark_tree_contents_uninteresting(revs->repo, tree);
///                 return NULL;
///         }
///         add_pending_object_with_path(revs, object, name, mode, path);
///         return NULL;
/// }
/// ```
///
/// Returns whether the operand was claimed. It is claimed whatever `--objects`
/// says — the `!revs->tree_objects` guard drops the object, it does not turn the
/// argument back into an error — which is why `git rev-list main^{tree}` exits 0
/// with no output at all rather than reporting an unknown revision.
///
/// The recorded name is `oc.path`, the path arm the operand read the object out
/// of, and it is empty for a peel such as `main^{tree}`. `traverse_non_commits()`
/// uses it as the base for everything below a tree.
fn pend_non_commit(
    repo: &gix::Repository,
    spec: &str,
    negate: bool,
    pending: &mut Vec<Pending>,
) -> bool {
    let bare = spec.strip_prefix('^').unwrap_or(spec);
    let Some(id) = crate::objname::resolve_quiet(repo, bare) else {
        return false;
    };
    let Ok(object) = repo.find_object(id) else {
        return false;
    };
    if !matches!(object.kind, gix::object::Kind::Tree | gix::object::Kind::Blob) {
        return false;
    }
    let name = operand_path(repo, bare);
    pending.push(Pending { id, name, kind: object.kind, uninteresting: negate });
    true
}

/// `oc.path`, which `get_oid_with_context()` fills in only for the path arm — the
/// path the object was reached through, and empty for every other spelling.
///
/// `handle_commit()` carries it into `add_pending_object_with_path()`, and
/// `traverse_non_commits()` uses it as the base for everything under a tree, so
/// `git rev-list --objects main:sub` names its entries `sub` and `sub/s.txt`
/// rather than `` and `s.txt`.
fn operand_path(repo: &gix::Repository, spec: &str) -> Vec<u8> {
    let Ok(canonical) = crate::objpath::canonical_paths(repo, spec) else {
        return Vec::new();
    };
    match crate::objpath::split(canonical.as_ref()) {
        crate::objpath::Split::Index { path, .. } | crate::objpath::Split::Tree { path, .. } => {
            path.as_bytes().to_vec()
        }
        _ => Vec::new(),
    }
}

/// git's `--bisect` pseudo-option: seed from the bisect refs.
///
/// `refs/bisect/<term_bad>*` are the tips and `refs/bisect/<term_good>*` are
/// excluded, with the terms coming from `BISECT_TERMS` when a session renamed
/// them (`read_bisect_terms`, defaults `bad`/`good`). Both are prefix matches,
/// which is how the per-commit `good-<oid>` refs are picked up.
pub(super) fn bisect_ref_tips(
    repo: &gix::Repository,
) -> anyhow::Result<Vec<(ObjectId, bool)>> {
    let terms = std::fs::read_to_string(repo.path().join("BISECT_TERMS")).unwrap_or_default();
    let mut lines = terms.lines();
    let term_bad = lines.next().filter(|l| !l.is_empty()).unwrap_or("bad");
    let term_good = lines.next().filter(|l| !l.is_empty()).unwrap_or("good");

    let mut out = Vec::new();
    for reference in repo.references()?.all()? {
        let Ok(reference) = reference else { continue };
        let full = reference.name().as_bstr().to_string();
        let Some(rest) = full.strip_prefix("refs/bisect/") else {
            continue;
        };
        let excluded = if rest.starts_with(term_bad) {
            false
        } else if rest.starts_with(term_good) {
            true
        } else {
            continue;
        };
        let target = match reference.try_id() {
            Some(id) => id.detach(),
            None => match reference.into_fully_peeled_id() {
                Ok(id) => id.detach(),
                Err(_) => continue,
            },
        };
        if let Some(commit) = repo.find_object(target).ok().and_then(|o| o.peel_to_commit().ok()) {
            out.push((commit.id, excluded));
        }
    }
    Ok(out)
}

/// git's `--bisect` pseudo-option for `rev-list`, which also records the tag
/// objects it peeled through so `--objects` can list them.
fn seed_bisect_refs(
    repo: &gix::Repository,
    negate: bool,
    seeds: &mut Vec<Seed>,
    pending: &mut Vec<Pending>,
) -> Result<(), String> {
    let terms = std::fs::read_to_string(repo.path().join("BISECT_TERMS")).unwrap_or_default();
    let mut lines = terms.lines();
    let term_bad = lines.next().filter(|l| !l.is_empty()).unwrap_or("bad");
    let term_good = lines.next().filter(|l| !l.is_empty()).unwrap_or("good");

    let refs = repo.references().map_err(|e| e.to_string())?;
    for reference in refs.all().map_err(|e| e.to_string())? {
        let reference = reference.map_err(|e| e.to_string())?;
        let full = reference.name().as_bstr().to_string();
        let Some(rest) = full.strip_prefix("refs/bisect/") else {
            continue;
        };
        let excluded = if rest.starts_with(term_bad) {
            false
        } else if rest.starts_with(term_good) {
            true
        } else {
            continue;
        };
        let target = match reference.try_id() {
            Some(id) => id.detach(),
            None => match reference.into_fully_peeled_id() {
                Ok(id) => id.detach(),
                Err(_) => continue,
            },
        };
        if let Some(id) = peel_recording_tags(repo, target, pending) {
            // The good refs are handed `*flags ^ (UNINTERESTING | BOTTOM)`.
            let uninteresting = excluded != negate;
            seeds.push(Seed {
                id,
                uninteresting,
                symmetric_left: false,
                bottom: uninteresting,
            });
        }
    }
    Ok(())
}

/// git's `find_bisection`: the commit that splits the range most evenly.
///
/// A commit's weight is the number of commits it reaches, itself included. The
/// search returns the first commit whose weight is within one of half the range
/// (`approx_halfway`), and otherwise the commit with the best split
/// (`best_bisection`). Merges are weighed by the explicit `count_distance()`
/// walk first, because their parents' reaches overlap and cannot be summed;
/// everything else inherits `parent + 1` in the cheap fill-in pass.
///
/// A TREESAME commit — one a pathspec limit walks past — changes nothing, so git
/// never *picks* one and never counts one: it is left out of `all`, adds nothing
/// to a `count_distance()` walk it is on, and inherits its parent's weight
/// unchanged instead of parent + 1.
///
/// `find_all` is `FIND_BISECTION_ALL`: the halfway shortcut is disabled (every
/// weight is computed) and the answer is the whole range sorted by
/// `best_bisection_sorted()`'s key rather than the single best commit.
fn find_bisection(
    commits: &[ObjectId],
    parents_of: &HashMap<ObjectId, Vec<ObjectId>>,
    first_parent_only: bool,
    find_all: bool,
    treesame: &HashSet<ObjectId>,
) -> Bisection {
    // git reverses the list while counting, so the oldest commit comes first.
    // Only the tree-changing commits are counted: `if (!(flags & TREESAME)) nr++`.
    let list: Vec<ObjectId> = commits.iter().rev().copied().collect();
    let nr = list.iter().filter(|id| !treesame.contains(*id)).count() as i64;
    if nr == 0 {
        return Bisection::default();
    }
    let slot: HashMap<ObjectId, usize> = list.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    // A parent outside the list is one the walk marked UNINTERESTING.
    let interesting = |id: &ObjectId| slot.contains_key(id);
    let parents = |id: &ObjectId| -> &[ObjectId] {
        let all = parents_of.get(id).map(Vec::as_slice).unwrap_or(&[]);
        if first_parent_only && !all.is_empty() {
            &all[..1]
        } else {
            all
        }
    };
    // `approx_halfway`: within one of the midpoint, or within ~0.1% of it.
    let approx_halfway = |w: i64| {
        let diff = 2 * w - nr;
        (-1..=1).contains(&diff) || diff.abs() < nr / 1024
    };

    // -1: one interesting parent, weight still to compute. -2: a merge, whose
    // weight needs the explicit walk.
    let mut weights: Vec<i64> = vec![0; list.len()];
    let mut counted = 0i64;
    for (n, id) in list.iter().enumerate() {
        match parents(id).iter().filter(|p| interesting(p)).count() {
            // A root reaches only itself — unless it is TREESAME, which reaches
            // nothing at all and keeps the zero weight it was allocated with.
            0 => {
                weights[n] = i64::from(!treesame.contains(id));
                counted += 1;
            }
            1 => weights[n] = -1,
            _ => weights[n] = -2,
        }
    }

    for n in 0..list.len() {
        if weights[n] != -2 {
            continue;
        }
        let mut visited: HashSet<ObjectId> = HashSet::new();
        weights[n] =
            count_distance(list[n], parents_of, &slot, &mut visited, first_parent_only, treesame);
        if !find_all && approx_halfway(weights[n]) {
            return Bisection::single(list[n], weights[n], nr);
        }
        counted += 1;
    }

    while counted < nr {
        let mut progressed = false;
        for n in 0..list.len() {
            if weights[n] >= 0 {
                continue;
            }
            // The first interesting parent whose weight is already known.
            let known = parents(&list[n]).iter().find_map(|p| {
                let i = *slot.get(p)?;
                (weights[i] >= 0).then_some(weights[i])
            });
            let Some(w) = known else { continue };
            counted += 1;
            progressed = true;
            // A TREESAME commit reaches exactly what its parent does, and the
            // halfway shortcut is not even tried on one (git checks it only in the
            // `else` arm).
            if treesame.contains(&list[n]) {
                weights[n] = w;
                continue;
            }
            weights[n] = w + 1;
            if !find_all && approx_halfway(weights[n]) {
                return Bisection::single(list[n], weights[n], nr);
            }
        }
        if !progressed {
            break;
        }
    }

    // `best_bisection_sorted()`: every candidate, keyed on the same distance the
    // single-answer path maximises, ordered by descending distance and — for the
    // ties that are the normal case — ascending object id (`compare_commit_dist`).
    // `reaches` stays the *weight* of the commit that comes out first, not its
    // distance, since git reads it back off the head of the list it returns.
    if find_all {
        let mut sorted: Vec<(ObjectId, i64, i64)> = list
            .iter()
            .enumerate()
            .filter(|(_, id)| !treesame.contains(*id))
            .map(|(n, id)| (*id, weights[n].min(nr - weights[n]), weights[n]))
            .collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let reaches = sorted.first().map_or(0, |(_, _, w)| *w);
        return Bisection {
            commits: sorted.into_iter().map(|(id, dist, _)| (id, dist)).collect(),
            reaches,
            all: nr,
        };
    }

    // `best_bisection`: the commit whose smaller side is largest, TREESAME
    // commits skipped since picking one would name a commit the pathspec says
    // changed nothing.
    let mut best = list[0];
    let mut best_weight = weights[0];
    let mut best_distance = -1i64;
    for (n, id) in list.iter().enumerate() {
        if treesame.contains(id) {
            continue;
        }
        let distance = weights[n].min(nr - weights[n]);
        if distance > best_distance {
            best = *id;
            best_weight = weights[n];
            best_distance = distance;
        }
    }
    Bisection::single(best, best_weight, nr)
}

/// What `find_bisection()` hands back: the commits to show and the two
/// out-parameters `show_bisect_vars()` reads — `reaches`, the weight of the first
/// of them, and `all`, the size of the range that was searched.
#[derive(Default)]
struct Bisection {
    /// Each commit with the distance `best_bisection_sorted()` weighed it at.
    /// Without `--bisect-all` this is the one chosen commit, whose distance is
    /// never printed.
    commits: Vec<(ObjectId, i64)>,
    reaches: i64,
    all: i64,
}

impl Bisection {
    /// The single-commit answer, which is every path but `FIND_BISECTION_ALL`.
    fn single(id: ObjectId, weight: i64, all: i64) -> Self {
        Self { commits: vec![(id, weight.min(all - weight))], reaches: weight, all }
    }
}

/// Port of `estimate_bisect_steps()` (bisect.c): how many more rounds a range of
/// `all` commits is expected to take.
///
/// ```c
/// int estimate_bisect_steps(int all)
/// {
///         int n, x, e;
///
///         if (all < 3)
///                 return 0;
///
///         n = log2u(all);
///         e = 1 << n;
///         x = all - e;
///
///         return (e < 3 * x) ? n : n - 1;
/// }
/// ```
fn estimate_bisect_steps(all: i64) -> i64 {
    if all < 3 {
        return 0;
    }
    let n = i64::from(63 - (all as u64).leading_zeros());
    let e = 1i64 << n;
    let x = all - e;
    if e < 3 * x {
        n
    } else {
        n - 1
    }
}

/// git's `count_distance`: how many listed commits `start` reaches, itself
/// included, counting each at most once across the whole walk.
fn count_distance(
    start: ObjectId,
    parents_of: &HashMap<ObjectId, Vec<ObjectId>>,
    slot: &HashMap<ObjectId, usize>,
    visited: &mut HashSet<ObjectId>,
    first_parent_only: bool,
    treesame: &HashSet<ObjectId>,
) -> i64 {
    let mut nr = 0;
    let mut cur = Some(start);
    while let Some(id) = cur {
        if !slot.contains_key(&id) || !visited.insert(id) {
            break;
        }
        // `if (!(commit->object.flags & TREESAME)) nr++;` — a commit the pathspec
        // walked past is still traversed, it just does not count.
        if !treesame.contains(&id) {
            nr += 1;
        }
        let parents = parents_of.get(&id).map(Vec::as_slice).unwrap_or(&[]);
        cur = parents.first().copied();
        if first_parent_only {
            continue;
        }
        // A merge's extra parents are separate strands, counted recursively.
        for extra in parents.iter().skip(1) {
            nr += count_distance(*extra, parents_of, slot, visited, first_parent_only, treesame);
        }
    }
    nr
}

/// The `--filter=<spec>` forms this port applies. Anything else — `sparse:`,
/// `object:type=`, `combine:` — is reported as an invalid spec rather than
/// silently letting every object through.
fn parse_filter(spec: &str) -> Option<Filter> {
    if spec == "blob:none" {
        return Some(Filter::BlobNone);
    }
    if let Some(v) = spec.strip_prefix("blob:limit=") {
        return parse_size(v).map(Filter::BlobLimit);
    }
    if let Some(v) = spec.strip_prefix("tree:") {
        return v.parse::<u64>().ok().map(Filter::TreeDepth);
    }
    None
}

/// git's `git_parse_ulong` for a filter size: digits with an optional `k`/`m`/`g`
/// multiplier, either case.
fn parse_size(value: &str) -> Option<u64> {
    let (digits, scale) = match value.as_bytes().last() {
        Some(b'k') | Some(b'K') => (&value[..value.len() - 1], 1024),
        Some(b'm') | Some(b'M') => (&value[..value.len() - 1], 1024 * 1024),
        Some(b'g') | Some(b'G') => (&value[..value.len() - 1], 1024 * 1024 * 1024),
        _ => (value, 1),
    };
    digits.parse::<u64>().ok()?.checked_mul(scale)
}

/// The commit's committer timestamp, or 0 when the object cannot be read — the
/// value git's `parse_commit_gently` failure path leaves behind.
pub(crate) fn commit_date(repo: &gix::Repository, id: ObjectId) -> i64 {
    let Ok(object) = repo.find_object(id) else {
        return 0;
    };
    if object.kind != gix::object::Kind::Commit {
        return 0;
    }
    object.into_commit().time().map(|t| t.seconds).unwrap_or(0)
}

/// `record_author_date()`: the `author` header's timestamp, or 0 when the commit
/// has no author line or its date does not parse.
fn author_date(repo: &gix::Repository, id: ObjectId) -> i64 {
    let author = || -> Option<i64> {
        let commit = repo.find_object(id).ok()?.try_into_commit().ok()?;
        Some(commit.author().ok()?.time().ok()?.seconds)
    };
    author().unwrap_or(0)
}

/// The commit's parent ids, empty when the object cannot be read.
pub(crate) fn commit_parents(repo: &gix::Repository, id: ObjectId) -> Vec<ObjectId> {
    let Ok(object) = repo.find_object(id) else {
        return Vec::new();
    };
    if object.kind != gix::object::Kind::Commit {
        return Vec::new();
    }
    object
        .into_commit()
        .parent_ids()
        .map(|p| p.detach())
        .collect()
}

/// Everything `roots` reaches through `parents_of`, the roots included.
///
/// This is how git's flag propagation behaves: `process_parents` ORs the
/// inherited flags onto every parent it walks, so a flag set on a tip ends up on
/// its whole ancestry.
fn reachable_from(
    roots: &[ObjectId],
    parents_of: &HashMap<ObjectId, Vec<ObjectId>>,
) -> HashSet<ObjectId> {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut stack: Vec<ObjectId> = roots.to_vec();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        for parent in parents_of.get(&id).into_iter().flatten() {
            if !seen.contains(parent) {
                stack.push(*parent);
            }
        }
    }
    seen
}

/// git's `limit_to_ancestry`: keep only the commits that can reach a bottom
/// commit, marking bottom-up until no more progress is made.
///
/// Shared with `fast-export`, which reaches `limit_list` through the same
/// `setup_revisions` and so has to apply the identical filter to its own walk.
pub(super) fn limit_to_ancestry(
    bottoms: &[ObjectId],
    list: &[ObjectId],
    parents_of: &HashMap<ObjectId, Vec<ObjectId>>,
) -> Vec<ObjectId> {
    let mut marked: HashSet<ObjectId> = bottoms.iter().copied().collect();
    let mut progress = true;
    while progress {
        progress = false;
        // Reversed so a parent is usually settled before its children are asked.
        for id in list.iter().rev() {
            if marked.contains(id) {
                continue;
            }
            if parents_of
                .get(id)
                .into_iter()
                .flatten()
                .any(|p| marked.contains(p))
            {
                marked.insert(*id);
                progress = true;
            }
        }
    }
    list.iter()
        .copied()
        .filter(|id| marked.contains(id))
        .collect()
}

/// git's `rewrite_parents` over `rewrite_one`, then `remove_duplicate_parents`.
///
/// Under a path limit a parent that is TREESAME — here, one the walk visited but
/// the path filter dropped — is replaced by the ancestor the simplification kept,
/// following `one_relevant_parent` at each step. A chain that runs into a root
/// commit loses the parent entirely (`rewrite_one_noparents`), and a parent the
/// walk never visited is UNINTERESTING and stays as it is.
fn rewrite_parents(
    id: &ObjectId,
    survivors: &HashSet<ObjectId>,
    parents_of: &HashMap<ObjectId, Vec<ObjectId>>,
    first_parent: bool,
) -> Vec<ObjectId> {
    // `relevant_commit`: a parent outside the walk is UNINTERESTING, and only a
    // BOTTOM one of those counts as relevant — which a rewritten chain never is.
    let relevant = |c: &ObjectId| parents_of.contains_key(c);
    // `one_relevant_parent`: the first parent when there is only one (or under
    // `--first-parent`), otherwise the sole relevant parent, if there is exactly
    // one.
    let one_relevant = |parents: &[ObjectId]| -> Option<ObjectId> {
        if parents.is_empty() {
            return None;
        }
        if first_parent || parents.len() == 1 {
            return Some(parents[0]);
        }
        let mut sole = None;
        for p in parents {
            if relevant(p) {
                if sole.is_some() {
                    return None;
                }
                sole = Some(*p);
            }
        }
        sole
    };

    let mut out: Vec<ObjectId> = Vec::new();
    for start in parents_of.get(id).into_iter().flatten() {
        let mut p = *start;
        let keep = loop {
            let Some(parents) = parents_of.get(&p) else {
                // UNINTERESTING: `rewrite_one` stops and keeps it.
                break Some(p);
            };
            if survivors.contains(&p) {
                // Not TREESAME: this is the ancestor the rewrite was looking for.
                break Some(p);
            }
            if parents.is_empty() {
                break None;
            }
            match one_relevant(parents) {
                Some(next) => p = next,
                None => break Some(p),
            }
        };
        // `remove_duplicate_parents`: two rewritten parents can land on the same
        // commit, and git keeps only the first.
        if let Some(keep) = keep {
            if !out.contains(&keep) {
                out.push(keep);
            }
        }
    }
    out
}

/// git's `--boundary` list, shared by every command that offers the flag:
/// `log`, `show`, `whatchanged`, `rev-list`, `shortlog` and the prerequisite
/// lines of `bundle create` (which is `revs.boundary = 1` over the same
/// machinery, bundle.c:590-601).
///
/// **Membership.** `get_revision_internal()` marks *every* parent of every
/// commit it returns, not only the ones a `^rev` hid: `for (l = c->parents; l;
/// l = l->next) { if (p->flags & (CHILD_SHOWN | SHOWN)) continue; p->flags |=
/// CHILD_SHOWN; add_object_array(p, NULL, &revs->boundary_commits); }`
/// (revision.c:4583-4591). A parent kept out of the output by `--merges`,
/// `--no-merges`, a parent-count bound, a date limit or a header grep is never
/// *returned*, so it never gains SHOWN and stays on the list —
/// `create_boundary_commit_list()` drops only the ones that were shown
/// (revision.c:4494-4497). `shown` here is what the command actually emitted,
/// which is also why a `--skip`ped or `--max-count`-truncated commit marks
/// nothing: `get_revision_1()`'s result is discarded before the loop above runs.
///
/// **Order.** `create_boundary_commit_list()` walks `revs->boundary_commits` in
/// marking order but splices each survivor onto the *front* of `revs->commits`
/// with `commit_list_insert()` (revision.c:4490-4500), so the list it hands to
/// `sort_in_topological_order(&revs->commits, revs->sort_order)`
/// (revision.c:4506) is in **reverse marking order**. That sort seeds its queue
/// in list order and, for the default `REV_SORT_IN_GRAPH_ORDER`, reverses the
/// seed so a LIFO pop reproduces it (commit.c:1015-1016 with the
/// `compare == NULL` stack in prio-queue.c) — the tips therefore come out in
/// list order, i.e. reverse marking order. `--topo-order` keeps that sort order
/// (revision.c:2437); only `--date-order` (`REV_SORT_BY_COMMIT_DATE`,
/// revision.c:2454) replaces it, which is what `by_date` selects.
///
/// `parents_of` is filled in for the boundary commits themselves — the walk
/// never visited them, and both the sort here and `--parents` need the links.
pub(crate) fn boundary_list(
    repo: &gix::Repository,
    shown: &[ObjectId],
    parents_of: &mut HashMap<ObjectId, Vec<ObjectId>>,
    by_date: bool,
) -> Vec<ObjectId> {
    let shown_set: HashSet<ObjectId> = shown.iter().copied().collect();
    let mut candidates: Vec<ObjectId> = Vec::new();
    let mut child_shown: HashSet<ObjectId> = HashSet::new();
    for id in shown {
        for parent in parents_of.get(id).into_iter().flatten() {
            if child_shown.insert(*parent) {
                candidates.push(*parent);
            }
        }
    }
    let list: Vec<ObjectId> = candidates
        .into_iter()
        .rev()
        .filter(|id| !shown_set.contains(id))
        .collect();
    for id in &list {
        parents_of
            .entry(*id)
            .or_insert_with(|| commit_parents(repo, *id));
    }
    let dates: Option<HashMap<ObjectId, i64>> = by_date.then(|| {
        list.iter()
            .map(|id| (*id, commit_date(repo, *id)))
            .collect()
    });
    topo_sort(&list, parents_of, dates.as_ref())
}

/// git's `print_disk_usage` under `--disk-usage=human`: `strbuf_humanise_bytes`.
fn human_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;
    if bytes >= TIB {
        format!("{:.2} TiB", bytes as f64 / TIB as f64)
    } else if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} bytes")
    }
}

/// Resolve `spec` to the commit it names, recording any annotated tag peeled
/// through on the way. `None` means the spec does not name a commit.
///
/// Silent by design. `get_oid_basic()` has two warnings — the ambiguous 40-hex
/// refname and the reflog that does not reach back far enough — and they come out
/// of one call, in one order, with one short-circuit rule across a range's two
/// endpoints. [`super::log::warn_operand`] is that whole rule, and it runs once at
/// the site that reads an operand off argv or stdin; this is called several times
/// per operand (both endpoints, then again to diagnose a failure), so warning here
/// would multiply them.
fn resolve(
    repo: &gix::Repository,
    spec: &str,
    pending: &mut Vec<Pending>,
) -> Option<ObjectId> {
    // `get_oid_basic()` reads a `<ref>@{…}` operand with `repo_dwim_log()` and
    // `read_ref_at()` (`object-name.c:742-789`), never with the revspec grammar,
    // and the two disagree: gitoxide answers with the selected entry's raw *new*
    // id where `read_ref_at()` keeps the ref's current value — the null id after a
    // `git branch -m` round trip. See [`crate::objname::reflog_oid`].
    // The test is on the *reduced* name: a `^{…}`, `~<n>` or `:<path>` suffix is
    // applied to what the reader answered, never folded into the selector. See
    // [`crate::objname::reflog_spec_oid`].
    // The operand's `oc.path` travels with it: `handle_revision_arg()` reads it
    // out of `get_oid_with_context()` and hands it to `handle_commit()`, whose
    // tree and blob arms pend under it. Without it a `<rev>:<path>` operand
    // reached its tree arm with an empty path and the walk under that tree was
    // named from the root — `s.txt` where git says `sub/s.txt`.
    let path = operand_path(repo, spec);
    if crate::objname::resolves_through_reflog(spec) {
        return crate::objname::reflog_spec_oid(repo, spec)
            .and_then(|id| peel_recording_tags_at(repo, id, &path, pending));
    }
    // `at_mark()` compares with `strncasecmp`, so `main@{PUSH}` is the same
    // operand as `main@{push}`; gitoxide's parser is case-sensitive.
    let id = repo.rev_parse_single(crate::objname::canonical_spec(repo, spec).as_ref()).ok()?.detach();
    peel_recording_tags_at(repo, id, &path, pending)
}

/// Peel `id` down to a commit, pushing every tag object passed through onto
/// `pending` under its own name — which is what `--objects` reports for them.
///
/// ```c
/// while (object->type == OBJ_TAG) {
///         struct tag *tag = (struct tag *) object;
///         if (revs->tag_objects && !(flags & UNINTERESTING))
///                 add_pending_object(revs, object, tag->tag);
///         …
///         object = parse_object(revs->repo, get_tagged_oid(tag));
///         …
///         /*
///          * We'll handle the tagged object by looping or dropping
///          * through to the non-tag handlers below. Do not
///          * propagate path data from the tag's pending entry.
///          */
///         path = NULL;
///         mode = 0;
/// }
/// ```
///
/// (`handle_commit()`, revision.c.) A chain that ends at a tree or a blob is not
/// a seed for the commit walk, but it is still an object to list — pended with
/// *no* path, which is why `git rev-list --objects --all` prints a tagged blob
/// with an empty name rather than the one its tree gives it.
fn peel_recording_tags(
    repo: &gix::Repository,
    id: ObjectId,
    pending: &mut Vec<Pending>,
) -> Option<ObjectId> {
    peel_recording_tags_at(repo, id, &[], pending)
}

/// [`peel_recording_tags`] for an operand that carries an `oc.path`.
///
/// `path` is what `handle_revision_arg()` read out of `get_oid_with_context()`
/// and is the base a pended tree's contents are named from. The C resets it in
/// the tag loop — "Do not propagate path data from the tag's pending entry",
/// `path = NULL` — so a chain that peels through a tag lands on its tree with no
/// path, exactly as if the tree had been named on its own.
fn peel_recording_tags_at(
    repo: &gix::Repository,
    id: ObjectId,
    path: &[u8],
    pending: &mut Vec<Pending>,
) -> Option<ObjectId> {
    let mut id = id;
    let mut path = path.to_vec();
    loop {
        let object = repo.find_object(id).ok()?;
        let kind = object.kind;
        match kind {
            gix::object::Kind::Commit => return Some(id),
            gix::object::Kind::Tree | gix::object::Kind::Blob => {
                pending.push(Pending {
                    id,
                    name: std::mem::take(&mut path),
                    kind,
                    uninteresting: false,
                });
                return None;
            }
            gix::object::Kind::Tag => {
                let tag = object.into_tag();
                let tag_id = tag.id;
                let (name, target) = {
                    let decoded = tag.decode().ok()?;
                    (decoded.name.to_vec(), decoded.target())
                };
                pending.push(Pending {
                    id: tag_id,
                    name,
                    kind: gix::object::Kind::Tag,
                    uninteresting: false,
                });
                // `path = NULL; mode = 0;` — the tagged object is not reached
                // through the operand's path arm.
                path.clear();
                id = target;
            }
        }
    }
}

/// Drop repeats while keeping the first occurrence — git ignores a seed it has
/// already queued, so the earliest mention is the one that fixes the order.
fn dedup_in_place(ids: &mut Vec<ObjectId>) {
    let mut seen = HashSet::new();
    ids.retain(|id| seen.insert(*id));
}

/// git's `sort_in_topological_order`: emit no commit before all of its children.
///
/// `dates` selects the tie-break, which is git's `sort_order`. `None` is
/// `REV_SORT_IN_GRAPH_ORDER` — a LIFO stack, which keeps a branch contiguous and
/// is what `--topo-order` and the boundary list use. `Some(dates)` is
/// `REV_SORT_BY_COMMIT_DATE` for `--date-order`: a priority queue that takes the
/// newest ready commit, with insertion order breaking equal timestamps.
pub(crate) fn topo_sort(
    commits: &[ObjectId],
    parents_of: &HashMap<ObjectId, Vec<ObjectId>>,
    dates: Option<&HashMap<ObjectId, i64>>,
) -> Vec<ObjectId> {
    let lifo = dates.is_none();
    // Every listed commit starts at 1, then gains one per listed child.
    let mut indegree: HashMap<ObjectId, usize> = commits.iter().map(|id| (*id, 1usize)).collect();
    for id in commits {
        for parent in parents_of.get(id).into_iter().flatten() {
            if let Some(n) = indegree.get_mut(parent) {
                if *n != 0 {
                    *n += 1;
                }
            }
        }
    }

    // The tips are the commits no listed commit reaches.
    let mut queue: Vec<ObjectId> = commits
        .iter()
        .filter(|id| indegree.get(*id) == Some(&1))
        .copied()
        .collect();
    // git reverses the seed queue so that popping a LIFO stack still yields the
    // tips in traversal order.
    if lifo {
        queue.reverse();
    }

    let mut out = Vec::with_capacity(commits.len());
    while !queue.is_empty() {
        let id = if lifo {
            queue.pop()
        } else {
            // `prio_queue_get` with `compare_commits_by_commit_date` hands back
            // the newest commit, and ties go to whichever entered the queue first.
            let date_of = |id: &ObjectId| dates.and_then(|d| d.get(id)).copied().unwrap_or(0);
            let at = (0..queue.len())
                .max_by_key(|i| (date_of(&queue[*i]), -(*i as i64)))
                .unwrap_or(0);
            Some(queue.remove(at))
        };
        let Some(id) = id else { break };
        for parent in parents_of.get(&id).into_iter().flatten() {
            if let Some(n) = indegree.get_mut(parent) {
                if *n == 0 {
                    continue;
                }
                *n -= 1;
                if *n == 1 {
                    queue.push(*parent);
                }
            }
        }
        indegree.insert(id, 0);
        out.push(id);
    }
    out
}

/// git's `mark_edges_uninteresting`: record everything an excluded (`^rev`)
/// commit reaches as already-emitted, so `--objects` on `a..b` lists only b's
/// new data.
fn mark_hidden_objects(
    repo: &gix::Repository,
    hidden: &[ObjectId],
    first_parent: bool,
    seen: &mut HashSet<ObjectId>,
) -> Result<()> {
    let mut platform = repo
        .rev_walk(hidden.to_vec())
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
    if first_parent {
        platform = platform.first_parent_only();
    }
    for info in platform.all()? {
        if let Some(tree) = commit_tree(repo, info?.id) {
            mark_tree_seen(repo, tree, seen);
        }
    }
    Ok(())
}

/// Append the `--objects` lines for one commit: its tree walked depth-first,
/// globally de-duplicated through `seen`.
///
/// An object the repository does not have is fatal unless `--missing` asked for
/// it to be skipped, which is git's `finish_object__ma`; `absent` collects the
/// ones `--missing=print` then reports.
fn collect_commit_objects(
    repo: &gix::Repository,
    commit: ObjectId,
    seen: &mut HashSet<ObjectId>,
    lines: &mut Vec<(ObjectId, Vec<u8>)>,
    absent: &mut Vec<MissingObject>,
    omitted: &mut Vec<ObjectId>,
    walk: &ObjectWalk<'_>,
) -> Result<Result<(), ExitCode>> {
    let Some(tree) = commit_tree(repo, commit) else {
        return Ok(Ok(()));
    };
    if !seen.insert(tree) {
        return Ok(Ok(()));
    }
    // `tree:0` omits even the root tree; every other filter keeps it. A root tree
    // is reached through no path, so its name is empty.
    if matches!(walk.filter, Some(Filter::TreeDepth(0))) {
        omitted.push(tree);
    } else {
        lines.push((tree, Vec::new()));
    }
    walk_tree(repo, tree, &[], 1, seen, lines, absent, omitted, walk)
}

/// The tree a commit points at, or `None` if the object is missing or is not a
/// commit. Never panics: gix's `into_commit` would, and a panic reads as a crash.
fn commit_tree(repo: &gix::Repository, id: ObjectId) -> Option<ObjectId> {
    let object = repo.find_object(id).ok()?;
    if object.kind != gix::object::Kind::Commit {
        return None;
    }
    Some(object.into_commit().tree_id().ok()?.detach())
}

/// `rev_compare_tree()` for the shared simplification passes, over rev-list's own
/// tree comparison so there is one pathspec engine in play and not two.
struct PathDiff<'a> {
    repo: &'a gix::Repository,
    specs: &'a mut super::log::PathspecMatcher,
}

impl super::simplify::TreeDiff for PathDiff<'_> {
    fn differs(&mut self, commit: ObjectId, parent: Option<ObjectId>) -> Result<bool> {
        let Some(tree) = commit_tree(self.repo, commit) else {
            return Ok(false);
        };
        let parent_tree = parent.and_then(|id| commit_tree(self.repo, id));
        diff_touches_path(self.repo, parent_tree, tree, self.specs)
    }
}

/// The `REV_TREE_NEW` arm of `try_to_simplify_commit()` (revision.c):
///
/// ```c
/// case REV_TREE_NEW:
///         if (revs->remove_empty_trees &&
///             rev_same_tree_as_empty(revs, p)) {
///                 /* We are adding all the specified paths from this parent, so
///                  * the history beyond this parent is not interesting. Remove
///                  * its parents (they are grandparents for us). IOW, we pretend
///                  * this parent is a "root" commit. */
///                 p->parents = NULL;
///         }
/// ```
///
/// Returns every parent that is turned into a root that way. A parent that
/// *differs* from its child over the pathspec while carrying nothing the pathspec
/// matches can only differ by additions, which is exactly `REV_TREE_NEW` — so the
/// two-sided test here is the same decision, expressed with the tree comparison
/// this file already has.
///
/// The parent loop reproduces the shape of the C one, because which parents are
/// reached is part of the answer: `--first-parent` breaks at the second parent,
/// `!revs->dense` returns before a single-parent commit is compared at all, and a
/// TREESAME relevant parent ends the loop with a `return` while
/// `revs->simplify_history` is on (`--full-history` clears it and the loop runs on).
fn empty_tree_roots(
    repo: &gix::Repository,
    commits: &[ObjectId],
    parents_of: &HashMap<ObjectId, Vec<ObjectId>>,
    first_parent: bool,
    dense: bool,
    simplify_history: bool,
    specs: &mut super::log::PathspecMatcher,
) -> Result<HashSet<ObjectId>> {
    let walked: HashSet<ObjectId> = commits.iter().copied().collect();
    let mut roots: HashSet<ObjectId> = HashSet::new();
    for id in commits {
        let Some(tree) = commit_tree(repo, *id) else { continue };
        let parents = parents_of.get(id).map(Vec::as_slice).unwrap_or(&[]);
        // `if (!commit->parents) { … return; }` — a root has nothing to cut.
        if parents.is_empty() {
            continue;
        }
        // `if (!revs->dense && !commit->parents->next) return;`
        if !dense && parents.len() == 1 {
            continue;
        }
        let considered = if first_parent { &parents[..1] } else { parents };
        for p in considered {
            let parent_tree = commit_tree(repo, *p);
            if !diff_touches_path(repo, parent_tree, tree, specs)? {
                // REV_TREE_SAME. `relevant_commit(p)` is the walked set here, as
                // it is for [`treesame_parent`].
                if simplify_history && walked.contains(p) {
                    break;
                }
                continue;
            }
            match parent_tree {
                // `rev_same_tree_as_empty(revs, p)`: the parent against the empty
                // tree, under the same pathspec.
                Some(pt) if diff_touches_path(repo, None, pt, specs)? => {}
                _ => {
                    roots.insert(*p);
                }
            }
        }
    }
    Ok(roots)
}

/// git's TREESAME test for one commit under a path limit.
///
/// `None` means the commit differs from every parent considered and is shown.
/// `Some(parent)` means it is TREESAME and simplified away, naming the first
/// *relevant* parent it matched — the one `try_to_simplify_commit` collapses the
/// commit's parent list onto. `Some(None)` is a commit with no such parent: a
/// root whose tree holds no matching path, or one TREESAME only to a parent the
/// walk never visited.
///
/// A single-parent commit is TREESAME iff it does not differ from its parent for
/// the paths; a merge iff it does not differ from at least one parent, which is
/// git's default merge simplification. `--first-parent` limits a merge to its
/// first parent. This decides the shown set; it does not reproduce git's
/// traversal pruning, which only diverges for merges limited to a path that names
/// a real tracked file.
fn treesame_parent(
    repo: &gix::Repository,
    commit: ObjectId,
    parents: &[ObjectId],
    first_parent: bool,
    specs: &mut super::log::PathspecMatcher,
    walked: &HashMap<ObjectId, Vec<ObjectId>>,
    show_pulls: bool,
) -> Result<Option<Option<ObjectId>>> {
    let Some(tree) = commit_tree(repo, commit) else {
        return Ok(Some(None));
    };
    if parents.is_empty() {
        return Ok(match diff_touches_path(repo, None, tree, specs)? {
            true => None,
            false => Some(None),
        });
    }
    let considered = if first_parent { &parents[..1] } else { parents };
    let mut same: Option<Option<ObjectId>> = None;
    for (nth, parent) in considered.iter().enumerate() {
        let parent_tree = commit_tree(repo, *parent);
        if diff_touches_path(repo, parent_tree, tree, specs)? {
            continue;
        }
        // A parent outside the walk is UNINTERESTING, and `relevant_commit`
        // refuses to simplify onto one, so keep looking for a relevant match.
        if walked.contains_key(parent) {
            // `if (!revs->show_pulls || !nth_parent) commit->object.flags |=
            // TREESAME;`: matching a *later* parent makes this merge a diversion,
            // which `--show-pulls` keeps.
            if show_pulls && nth > 0 {
                return Ok(None);
            }
            return Ok(Some(Some(*parent)));
        }
        same = Some(None);
    }
    Ok(same)
}

/// Whether the diff turning `old_tree` (empty when `None`) into `new_tree` touches
/// any of `pathspecs`. Rename tracking is off so a rename shows as a deletion and
/// an addition, letting either endpoint's path match.
fn diff_touches_path(
    repo: &gix::Repository,
    old_tree: Option<ObjectId>,
    new_tree: ObjectId,
    specs: &mut super::log::PathspecMatcher,
) -> Result<bool> {
    let Some(new) = tree_object(repo, new_tree) else {
        return Ok(false);
    };
    let old = old_tree
        .and_then(|id| tree_object(repo, id))
        .unwrap_or_else(|| repo.empty_tree());

    // The diff has to be RECURSIVE for the pathspec to see real file paths: a
    // tree-level walk reports `src` where the change is `src/gen/table.rs`, and
    // `:(exclude)src/gen` then fails to exclude anything, because `src` itself is
    // not what the spec names. `diff_tree_to_tree` is the same file-granular diff
    // `log` decides TREESAME with, so the two can never disagree.
    let changes = repo
        .diff_tree_to_tree(Some(&old), Some(&new), gix::diff::Options::default())
        .map_err(|e| anyhow!("{e}"))?;

    Ok(changes
        .iter()
        .filter(|c| !super::log::change_is_tree(c))
        .any(|c| specs.matches(super::log::change_path(c))))
}

/// The entries of a tree object, or `None` if it is missing or not a tree.
fn tree_object(repo: &gix::Repository, id: ObjectId) -> Option<gix::Tree<'_>> {
    let object = repo.find_object(id).ok()?;
    if object.kind != gix::object::Kind::Tree {
        return None;
    }
    Some(object.into_tree())
}

/// Record `tree` and everything under it as already-emitted, without listing it.
fn mark_tree_seen(repo: &gix::Repository, tree: ObjectId, seen: &mut HashSet<ObjectId>) {
    if !seen.insert(tree) {
        return;
    }
    let Some(object) = tree_object(repo, tree) else {
        return;
    };
    for entry in object.iter() {
        let Ok(entry) = entry else { return };
        if entry.mode().is_commit() {
            continue;
        }
        let id = entry.object_id();
        if entry.mode().is_tree() {
            mark_tree_seen(repo, id, seen);
        } else {
            seen.insert(id);
        }
    }
}

/// Depth-first walk recording `(<oid>, <path>)` per entry, descending into a subtree
/// immediately after listing it — the order git's `process_tree` produces.
///
/// `depth` is the depth of the entries listed here, counting the root tree as 0
/// and its entries as 1, which is what `--filter=tree:<n>` measures. Gitlink
/// entries are skipped: their commit lives in another repository.
#[allow(clippy::too_many_arguments)]
fn walk_tree(
    repo: &gix::Repository,
    tree: ObjectId,
    base: &[u8],
    depth: u64,
    seen: &mut HashSet<ObjectId>,
    lines: &mut Vec<(ObjectId, Vec<u8>)>,
    absent: &mut Vec<MissingObject>,
    omitted: &mut Vec<ObjectId>,
    walk: &ObjectWalk<'_>,
) -> Result<Result<(), ExitCode>> {
    // Nothing at this depth, or under it, survives the tree filter.
    //
    // ```c
    // if (include_it)
    //         filter_res = LOFR_DO_SHOW;
    // else if (omits && !been_omitted)
    //         /*
    //          * Must update omit information of children
    //          * recursively; they have not been omitted yet.
    //          */
    //         filter_res = LOFR_ZERO;
    // else
    //         filter_res = LOFR_SKIP_TREE;
    // ```
    //
    // (`filter_trees_depth()`, list-objects-filter.c:226-235.) An excluded tree
    // is `LOFR_SKIP_TREE` — the subtree is never visited — *unless* an omit set
    // is being collected, in which case the walk descends anyway with nothing
    // shown, so every child is recorded as omitted too.
    let excluded = matches!(walk.filter, Some(Filter::TreeDepth(max)) if depth >= max);
    if excluded && !walk.collect_omits {
        return Ok(Ok(()));
    }
    let Some(object) = tree_object(repo, tree) else {
        // `base` is this tree's own path — the `path->buf` `process_tree()` hands
        // `show_object()` — and is empty for the root tree.
        if let Some(code) = note_missing(
            repo,
            tree,
            base,
            gix::object::Kind::Tree,
            absent,
            walk.missing,
        ) {
            return Ok(Err(code));
        }
        return Ok(Ok(()));
    };
    // The entries are collected first so the tree borrow ends before recursing.
    let entries: Vec<(ObjectId, Vec<u8>, gix::object::tree::EntryMode)> = object
        .iter()
        .filter_map(|e| e.ok())
        .map(|e| (e.object_id(), e.filename().to_vec(), e.mode()))
        .collect();
    for (id, filename, mode) in entries {
        if mode.is_commit() {
            continue;
        }
        let mut path = Vec::with_capacity(base.len() + 1 + filename.len());
        if !base.is_empty() {
            path.extend_from_slice(base);
            path.push(b'/');
        }
        path.extend_from_slice(&filename);
        if !entry_interesting(&path, mode.is_tree(), walk.pathspecs) {
            continue;
        }
        if !seen.insert(id) {
            continue;
        }
        if mode.is_tree() {
            match excluded {
                true => omitted.push(id),
                false => lines.push((id, path.clone())),
            }
            if let Err(code) =
                walk_tree(repo, id, &path, depth + 1, seen, lines, absent, omitted, walk)?
            {
                return Ok(Err(code));
            }
            continue;
        }
        if excluded {
            omitted.push(id);
            continue;
        }
        match blob_filtered(repo, id, &path, absent, walk)? {
            Ok(BlobVerdict::Filtered) => {
                omitted.push(id);
                continue;
            }
            Ok(BlobVerdict::Absent) => continue,
            Ok(BlobVerdict::Show) => {}
            Err(code) => return Ok(Err(code)),
        }
        lines.push((id, path));
    }
    Ok(Ok(()))
}

/// git's `missing_objects_map_entry`: one object the walk could not read, with
/// the two fields `--missing=print-info` renders beside its id.
///
/// ```c
/// struct missing_objects_map_entry {
///         struct oidmap_entry entry;
///         char *path;
///         unsigned type;
/// };
/// ```
///
/// (builtin/rev-list.c:90-94.) `path` is the name `show_object()` was called
/// with, and is empty for an object reached through no path at all — a root
/// tree, or a tip named on the command line — which is git's `NULL`. `type` is
/// `obj->type`, and git's `0` (nothing has looked the object up) is [`None`]
/// here; `print_missing_object` prints neither field when it is unset.
struct MissingObject {
    id: ObjectId,
    path: Vec<u8>,
    kind: Option<gix::object::Kind>,
}

/// One line of the missing-object section, as git's `print_missing_object()`.
///
/// ```c
/// if (line_term)
///         printf("?%s", oid_to_hex(&entry->entry.oid));
/// else
///         printf("%s%cmissing=yes", oid_to_hex(&entry->entry.oid), info_term);
///
/// if (!print_missing_info) {
///         putchar(line_term);
///         return;
/// }
///
/// if (entry->path && *entry->path) {
///         strbuf_addf(&sb, "%cpath=", info_term);
///         if (line_term) {
///                 quote_path(entry->path, NULL, &path, QUOTE_PATH_QUOTE_SP);
///                 strbuf_addbuf(&sb, &path);
///         } else {
///                 strbuf_addstr(&sb, entry->path);
///         }
/// }
/// if (entry->type)
///         strbuf_addf(&sb, "%ctype=%s", info_term, type_name(entry->type));
/// ```
///
/// (builtin/rev-list.c:154-190.) Only the `line_term` half is reachable here:
/// the `-z` that clears both terminators is `rev-list`'s own option and this
/// port does not carry it, so the separator is a space and every line ends in a
/// newline. An empty `path` prints no `path=` field at all — git's
/// `entry->path && *entry->path` — which is what a root tree or a tip named on
/// the command line leaves behind.
fn print_missing_object(entry: &MissingObject, print_missing_info: bool) -> Vec<u8> {
    let mut out = format!("?{}", entry.id).into_bytes();
    if !print_missing_info {
        out.push(b'\n');
        return out;
    }
    if !entry.path.is_empty() {
        out.extend_from_slice(b" path=");
        out.extend_from_slice(&quote_path_sp(&entry.path));
    }
    if let Some(kind) = entry.kind {
        out.extend_from_slice(b" type=");
        out.extend_from_slice(kind.as_bytes());
    }
    out.push(b'\n');
    out
}

/// `quote_path(in, NULL, out, QUOTE_PATH_QUOTE_SP)` (quote.c:350-372).
///
/// A `NULL` prefix leaves `relative_path()` returning the path unchanged, so the
/// only thing the flag adds over ordinary `write_name_quoted()` output is the
/// double-quote pair a path containing a space gets even when nothing in it
/// needs escaping — git wraps it itself and passes `CQUOTE_NODQ` so the escaper
/// does not add a second pair.
fn quote_path_sp(path: &[u8]) -> Vec<u8> {
    if !path.contains(&b' ') {
        return crate::quote::quoted_name_bytes(path);
    }
    let mut out = vec![b'"'];
    crate::quote::cq_body(path, &mut out);
    out.push(b'"');
    out
}

/// git's `finish_object__ma`: record or reject an object the repository lacks.
/// `Some(code)` means the walk must stop with that exit code.
fn note_missing(
    repo: &gix::Repository,
    id: ObjectId,
    path: &[u8],
    kind: gix::object::Kind,
    absent: &mut Vec<MissingObject>,
    missing: Missing,
) -> Option<ExitCode> {
    match missing {
        Missing::Error => Some(fatal(&format!("missing object '{id}'"))),
        Missing::AllowAny => None,
        // ```c
        // case MA_PRINT:
        // case MA_PRINT_INFO:
        //         add_missing_object_entry(&obj->oid, name, obj->type);
        //         return;
        // ```
        //
        // (builtin/rev-list.c:210-213.) One arm for both: the two actions record
        // the same entry and differ only where it is printed.
        Missing::Print | Missing::PrintInfo => {
            // `add_missing_object_entry()` returns early on an id the map already
            // holds, so the first path and type an object was seen under are the
            // ones reported.
            if !absent.iter().any(|entry| entry.id == id) {
                absent.push(MissingObject {
                    id,
                    path: path.to_vec(),
                    kind: Some(kind),
                });
            }
            None
        }
        // ```c
        // case MA_ALLOW_PROMISOR:
        //         if (is_promisor_object(the_repository, &obj->oid))
        //                 return;
        //         die("unexpected missing %s object '%s'", …);
        // ```
        //
        // (`finish_object__ma()`, builtin/rev-list.c:215-220.)
        Missing::AllowPromisor => match promisor_objects(repo).contains(&id) {
            true => None,
            false => Some(fatal(&format!("unexpected missing object '{id}'"))),
        },
    }
}

/// `is_promisor_object()` (packfile.c): every object a `.promisor` pack holds,
/// plus every object those reference — which is what makes a blob the pack's
/// trees point at "promised" even though it was never sent.
///
/// git builds the set once per process and keeps it; so does this, because the
/// walk asks about one object at a time.
pub(super) fn promisor_objects(repo: &gix::Repository) -> &'static HashSet<ObjectId> {
    static SET: std::sync::OnceLock<HashSet<ObjectId>> = std::sync::OnceLock::new();
    SET.get_or_init(|| {
        let mut set = promisor_pack_objects(repo);
        // `add_promisor_object()` walks each packed object and adds what it
        // names: a tree's entries, a commit's tree and parents, a tag's target.
        for id in set.clone() {
            let Ok(object) = repo.find_object(id) else { continue };
            match object.kind {
                gix::object::Kind::Tree => {
                    if let Ok(tree) = object.try_into_tree() {
                        if let Ok(iter) = tree.decode() {
                            set.extend(iter.entries.iter().map(|e| e.oid.to_owned()));
                        }
                    }
                }
                gix::object::Kind::Commit => {
                    if let Ok(commit) = object.try_into_commit() {
                        if let Ok(decoded) = commit.decode() {
                            set.insert(decoded.tree());
                            set.extend(decoded.parents());
                        }
                    }
                }
                gix::object::Kind::Tag => {
                    if let Ok(tag) = object.try_into_tag() {
                        if let Ok(target) = tag.target_id() {
                            set.insert(target.detach());
                        }
                    }
                }
                gix::object::Kind::Blob => {}
            }
        }
        set
    })
}

/// `repo_has_promisor_remote()` (promisor-remote.c:222-225), which is
/// `promisor_remote_init()` (:166-189) having found one: a `remote.<name>.promisor`
/// that is true, a `remote.<name>.partialclonefilter` at all — the filter alone
/// creates the entry (:146-160) — or the remote `extensions.partialClone` names.
/// A name beginning with `/` is refused (`promisor_remote_new()`, :74-78).
pub(super) fn has_promisor_remote(repo: &gix::Repository) -> bool {
    let snapshot = repo.config_snapshot();
    if let Some(sections) = snapshot.plumbing().sections_by_name("remote") {
        for section in sections {
            let Some(name) = section.header().subsection_name() else { continue };
            if name.starts_with(b"/") {
                continue;
            }
            if section.value("partialclonefilter").is_some() {
                return true;
            }
            let promisor = section
                .value("promisor")
                .and_then(|v| gix::config::Boolean::try_from(v.as_ref() as &gix::bstr::BStr).ok())
                .is_some_and(|b| b.0);
            if promisor {
                return true;
            }
        }
    }
    snapshot.string("extensions.partialClone").is_some()
}

/// Every object this repository's packs hold — git's `has_object_pack()` asked in
/// bulk, which is what `--unpacked` filters on.
pub(super) fn packed_objects(repo: &gix::Repository) -> HashSet<ObjectId> {
    pack_objects(repo, false)
}

/// The objects held by every pack with a `.promisor` file beside it — git's
/// `FOR_EACH_OBJECT_PROMISOR_ONLY` enumeration.
pub(super) fn promisor_pack_objects(repo: &gix::Repository) -> HashSet<ObjectId> {
    pack_objects(repo, true)
}

/// The ids in this repository's packs, optionally only the promisor ones.
fn pack_objects(repo: &gix::Repository, promisor_only: bool) -> HashSet<ObjectId> {
    let mut set = HashSet::new();
    let store = repo.objects.store_ref();
    for dir in std::iter::once(store.path().to_path_buf())
        .chain(store.alternate_db_paths().ok().into_iter().flatten())
    {
        let Ok(entries) = std::fs::read_dir(dir.join("pack")) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("idx")
                || (promisor_only && !path.with_extension("promisor").exists())
            {
                continue;
            }
            let Ok(index) = gix::odb::pack::index::File::at(&path, repo.object_hash()) else {
                continue;
            };
            set.extend(index.iter().map(|entry| entry.oid));
        }
    }
    set
}

/// git's `tree_entry_interesting`: an entry is listed when a pathspec names it or
/// an ancestor of it, and a tree is also listed when a pathspec could match
/// something underneath it, because the walk has to descend to reach that.
fn entry_interesting(
    path: &[u8],
    is_tree: bool,
    specs: Option<&super::log::PathspecMatcher>,
) -> bool {
    let Some(specs) = specs else { return true };
    if is_tree {
        specs.may_contain_match(path)
    } else {
        specs.matches(path)
    }
}

/// Whether `--filter=` omits this blob. Reading its header is also the missing
/// object check: `finish_object()` asks the object database for every object it
/// is about to show, and dies unless `--missing` said not to.
fn blob_filtered(
    repo: &gix::Repository,
    id: ObjectId,
    path: &[u8],
    absent: &mut Vec<MissingObject>,
    walk: &ObjectWalk<'_>,
) -> Result<Result<BlobVerdict, ExitCode>> {
    // ```c
    // if (ctx->filter_fn) {
    //         r = ctx->filter_fn(ctx->revs->repo, LOFS_BLOB, obj, …);
    //         if (r & LOFR_MARK_SEEN) obj->flags |= SEEN;
    //         if (r & LOFR_DO_SHOW) ctx->show_object(obj, path->buf, ctx->show_data);
    //         return;
    // }
    // ```
    //
    // (`process_blob()`, list-objects.c.) The filter runs *before* the show
    // callback, and only the callback looks the object up — so a `blob:none`
    // walk never touches a blob at all. That is what lets it list a partial
    // clone whose blobs are not there; asking about them first turned the
    // listing into `missing object '<oid>'`.
    if matches!(walk.filter, Some(Filter::BlobNone)) {
        return Ok(Ok(BlobVerdict::Filtered));
    }
    let header = repo.find_header(id).ok();
    let Some(header) = header else {
        if let Some(code) = note_missing(
            repo,
            id,
            path,
            gix::object::Kind::Blob,
            absent,
            walk.missing,
        ) {
            return Ok(Err(code));
        }
        // A missing object is skipped rather than listed — and it is *not* one
        // the filter omitted, so it belongs to the missing report, not the
        // omitted one.
        return Ok(Ok(BlobVerdict::Absent));
    };
    Ok(Ok(match walk.filter {
        Some(Filter::BlobLimit(limit)) if header.size() >= limit => BlobVerdict::Filtered,
        _ => BlobVerdict::Show,
    }))
}

/// What [`blob_filtered`] decided about one blob.
enum BlobVerdict {
    /// The filter omitted it: `--filter-print-omitted` names this one.
    Filtered,
    /// The repository does not have it; `--missing` has already been consulted.
    Absent,
    /// List it.
    Show,
}

/// git's `get_object_disk_usage`: the bytes the object occupies in the object
/// database — the loose file's size, or the packed entry's length.
///
/// `None` means the object could not be located at all, which git reports as
/// "unable to get disk usage of <oid>".
fn object_disk_size(repo: &gix::Repository, id: ObjectId) -> Option<u64> {
    let hex = id.to_string();
    let store = repo.objects.store_ref();
    for dir in std::iter::once(store.path().to_path_buf())
        .chain(store.alternate_db_paths().ok().into_iter().flatten())
    {
        // `do_oid_object_info_extended()` asks `find_pack_entry()` first and only
        // falls back to `loose_object_info()` when no pack has the object
        // (object-file.c). `repack` without `-d` leaves the loose copies behind,
        // so an object can be both — and the packed entry is the one git measures.
        if let Some(size) = packed_entry_size(&dir.join("pack"), id) {
            return Some(size);
        }
        let loose = dir.join(&hex[..2]).join(&hex[2..]);
        if let Ok(md) = std::fs::metadata(&loose) {
            return Some(md.len());
        }
    }
    None
}

/// The length of `id`'s entry inside a pack: the gap to the next entry in
/// offset order, which is what git's reverse index computes.
fn packed_entry_size(pack_dir: &std::path::Path, id: ObjectId) -> Option<u64> {
    // `sort_pack()` (packfile.c) puts local packs first and, among them, the
    // youngest first — "younger packs tend to contain more recent objects" — and
    // `find_pack_entry()` takes the first hit, so two packs holding the same
    // object are not interchangeable.
    let mut indexes: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(pack_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("idx"))
        .map(|path| {
            let mtime = std::fs::metadata(&path)
                .and_then(|md| md.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (mtime, path)
        })
        .collect();
    indexes.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    for (_, path) in indexes {
        let Ok(index) = gix::odb::pack::index::File::at(&path, id.kind()) else {
            continue;
        };
        let Some(entry_index) = index.lookup(id.as_ref()) else {
            continue;
        };
        let offset = index.pack_offset_at_index(entry_index);
        // Every entry's offset, so the next one after `offset` bounds this entry.
        let mut next = None;
        for other in 0..index.num_objects() {
            let candidate = index.pack_offset_at_index(other);
            if candidate > offset && next.is_none_or(|n| candidate < n) {
                next = Some(candidate);
            }
        }
        let end = match next {
            Some(n) => n,
            // The last entry runs to the pack trailer, which is one hash long.
            None => {
                let pack = path.with_extension("pack");
                std::fs::metadata(pack).ok()?.len() - id.kind().len_in_bytes() as u64
            }
        };
        return Some(end - offset);
    }
    None
}
