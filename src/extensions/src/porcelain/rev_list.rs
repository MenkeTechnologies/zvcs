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
}

/// What the `--objects` walk lists, and what it does about objects the
/// repository does not have.
#[derive(Clone, Copy)]
struct ObjectWalk<'a> {
    filter: Option<Filter>,
    missing: Missing,
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
///   * `--all` / `--branches` / `--tags` / `--remotes`, each with an optional
///     `=<pattern>`, and `--glob=<pattern>` — seed from a ref set, at the position
///     the flag appears
///   * `--exclude=<pattern>`          — drop refs from the next ref-set flag, and
///                                       only that one (`clear_ref_exclusions`)
///   * `--stdin`                      — read further revisions, and pathspecs
///                                       after a `--` line, from standard input
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
///   * `--grep=` / `--author=` / `--committer=` with `-i`/`-E`/`-F`/`-P`,
///     `--all-match` and `--invert-grep` — header and message predicates
///   * `-- <path>...`                 — path-limited: keep only commits whose diff
///                                       against a parent touched a matching path
///   * `-n <n>` / `-n<n>` / `--max-count=<n>` — limit the number listed
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
///   * `--[no-]commit-header`         — keep or drop the object-name line
///   * `--objects` / `--in-commit-order` / `--filter=<spec>` — also list the
///                                       trees and blobs reachable from the commits
///   * `--missing=(error|allow-any)`  — tolerate objects the repository lacks
///   * `--disk-usage[=human]`         — print the total on-disk size instead
/// ```
///
/// Genuinely unsupported forms stay rejected rather than silently accepted:
/// magic pathspecs (`:(glob)`, `:!exclude`, …), `--bisect`,
/// `--simplify-by-decoration`, `-z`, the `--missing` actions that need promisor
/// plumbing, and a `--cherry-mark` whose two sides are both non-empty, which is
/// the only case where git computes patch ids.
pub fn rev_list(args: &[String]) -> Result<ExitCode> {
    // `show_usage_if_asked(argc, argv, rev_list_usage)` (builtin/rev-list.c:711)
    // fires before the repository is opened, prints to stdout and exits 129 —
    // and only for a lone `-h`. Every other refusal is `usage()`, on stderr.
    if let Some(code) = super::show_usage_if_asked(args, USAGE) {
        return Ok(code);
    }

    let mut repo = match gix::discover(".") {
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
    let mut ancestry_path = false;
    let mut simplify_by_decoration = false;
    let mut bisect = false;
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
    let mut pathspecs: Vec<Vec<u8>> = Vec::new();
    // `setup_revisions()`'s `seen_dashdash`, found in a scan of the whole
    // argument vector before anything is resolved.
    let seen_dashdash = args.iter().any(|a| a == "--");
    let mut seeds: Vec<Seed> = Vec::new();
    // `--exclude=<glob>`, held until the next ref-selecting option consumes it.
    let mut ref_excludes: Vec<String> = Vec::new();
    // Commits the command line already caused to be parsed. Only `--no-walk`
    // cares — see [`super::log::no_walk_uninteresting`].
    let mut parsed_commits: HashSet<ObjectId> = HashSet::new();
    // Annotated tag objects encountered while peeling seeds. `--objects` lists
    // them, named by the tag's own name field, ahead of any tree.
    let mut pending_tags: Vec<(ObjectId, Vec<u8>)> = Vec::new();
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

    let mut i = 0;
    'args: while i < args.len() {
        let a = args[i].as_str();
        // How many revisions were already pending when this argument was reached,
        // so the `--no-walk` rule at the end of the iteration can see what it added.
        let seeds_before = seeds.len();
        // git's `parse_long_opt` takes a value attached (`--grep=x`) or detached
        // (`--grep x`); these are the rev-list options that carry one.
        for (name, sink) in [
            ("grep", &mut grep_pats as &mut Vec<String>),
            ("author", &mut author_pats),
            ("committer", &mut committer_pats),
        ] {
            if let Some(v) = long_opt_value(args, i, name) {
                sink.push(v.value);
                i += v.consumed;
                continue 'args;
            }
        }
        for name in ["since", "after"] {
            if let Some(v) = long_opt_value(args, i, name) {
                max_age = Some(approxidate(&v.value));
                i += v.consumed;
                continue 'args;
            }
        }
        for name in ["until", "before"] {
            if let Some(v) = long_opt_value(args, i, name) {
                min_age = Some(approxidate(&v.value));
                i += v.consumed;
                continue 'args;
            }
        }
        match a {
            "--count" => count_only = true,
            // git toggles this with `revs->reverse ^= 1`, so an even number of
            // `--reverse` flags cancels out and leaves the default order.
            "--reverse" => reverse = !reverse,
            "--first-parent" => first_parent = true,
            "--objects" => objects = true,
            "--object-names" => object_names = true,
            "--no-object-names" => object_names = false,
            "--in-commit-order" => in_commit_order = true,
            "--parents" => show_parents = true,
            "--children" => show_children = true,
            "--boundary" => boundary = true,
            "--left-right" => left_right = true,
            "--cherry-mark" => cherry_mark = true,
            "--ancestry-path" => ancestry_path = true,
            // `--simplify-by-decoration`: `simplify_commit()` keeps a decorated commit,
            // and — since simplification may not change the shape of the history — a
            // root or a merge; everything else is walked past.
            "--simplify-by-decoration" => simplify_by_decoration = true,
            "--bisect" => {
                if let Err(e) = seed_bisect_refs(&repo, negate, &mut seeds, &mut pending_tags) {
                    return Ok(fatal_text(&e));
                }
                rev_input_given = true;
                bisect = true;
            }
            "--topo-order" => order = Order::Topo,
            "--date-order" => order = Order::DateTopo,
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
            "--stdin" => read_stdin = true,
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
                let Some(n) = args.get(i) else {
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
                pathspecs.extend(args[i + 1..].iter().map(|s| s.as_bytes().to_vec()));
                break;
            }
            // `--exclude=<glob>` accumulates until the next ref-selecting option
            // consumes and clears it (`clear_ref_exclusions`); anything else in
            // between leaves the accumulation alone.
            "--exclude" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("error: option 'exclude' requires a value");
                    return Ok(usage_error());
                };
                ref_excludes.push(v.clone());
            }
            s if s.starts_with("--exclude=") => {
                ref_excludes.push(s["--exclude=".len()..].to_string());
            }
            "--glob" => {
                i += 1;
                let Some(v) = args.get(i) else {
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
                if let Err(e) = seed_ref_set(&repo, &sel, negate, &mut seeds, &mut pending_tags) {
                    return Ok(fatal_text(&e));
                }
                rev_input_given = true;
            }
            s if super::log::ref_selector(s).is_some() => {
                // Seeded in place: git processes a ref-set selector where it
                // appears, and with every commit sharing a timestamp the seed
                // order is what decides the output order.
                let (kind, pattern) = super::log::ref_selector(s).expect("checked above");
                let sel = super::log::RefSelection::new(
                    0,
                    kind,
                    pattern,
                    std::mem::take(&mut ref_excludes),
                    negate,
                );
                if let Err(e) = seed_ref_set(&repo, &sel, negate, &mut seeds, &mut pending_tags) {
                    return Ok(fatal_text(&e));
                }
                rev_input_given = true;
            }
            s if s.starts_with("--pretty=") || s.starts_with("--format=") => {
                let spec = s.split_once('=').expect("checked above").1;
                verbose_header = true;
                match get_commit_format(spec) {
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
            s if s.starts_with("--missing=") => match &s["--missing=".len()..] {
                "allow-any" => missing = Missing::AllowAny,
                "print" => missing = Missing::Print,
                // `print-info` reports each missing object's path and type through
                // `quote_path`, and `allow-promisor` consults the promisor remote;
                // neither has plumbing here.
                "print-info" | "allow-promisor" => {
                    return Ok(fatal(&format!("--missing={} is not supported", &s[10..])))
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
            // Every remaining flag is one git knows and this does not; a
            // revision never starts with `-`, so anything left is a usage error.
            s if s.starts_with('-') => return Ok(usage_error()),
            // `handle_revision_arg_1()`'s guard ahead of `handle_dotdot()`: a
            // bare `..` is the pathspec for the parent directory, never
            // `HEAD..HEAD`. `setup_revisions()` then sends it to
            // `append_prune_data()`, and the pathspec layer rejects it for
            // leaving the repository. See
            // [`crate::objname::is_parent_directory_pathspec`].
            s if crate::objname::is_parent_directory_pathspec(s, seen_dashdash) => {
                pathspecs.push(s.as_bytes().to_vec());
            }
            s => {
                if let Err(e) = seed_revision(&repo, s, negate, &mut seeds, &mut pending_tags) {
                    return Ok(fatal_text(&e));
                }
                note_parsed(&repo, s, &seeds[seeds_before..], &mut parsed_commits)?;
                rev_input_given = true;
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

    if show_parents && show_children {
        return Ok(fatal(
            "options '--parents' and '--children' cannot be used together",
        ));
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

    if read_stdin {
        // `read_revisions_from_stdin()` brackets its whole loop with
        // `cfg->warn_on_object_refname_ambiguity = 0`, so names arriving on stdin
        // never produce the ambiguity warning that the same names on argv do.
        let _quiet_ambiguity = crate::objname::AmbiguityWarnings::off();
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        // `read_revisions_from_stdin` reads revisions until a bare `--`, after
        // which every remaining line is a pathspec.
        let mut in_paths = false;
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            if in_paths {
                pathspecs.push(line.as_bytes().to_vec());
            } else if line == "--" {
                in_paths = true;
            } else {
                let seeds_before = seeds.len();
                if let Err(e) = seed_revision(&repo, line, negate, &mut seeds, &mut pending_tags) {
                    return Ok(fatal_text(&e));
                }
                note_parsed(&repo, line, &seeds[seeds_before..], &mut parsed_commits)?;
                // The same `handle_revision_arg()` reads these lines, so an
                // exclusion arriving on stdin cancels `--no-walk` just as one on
                // the command line does.
                if seeds[seeds_before..].iter().any(|s| s.uninteresting) {
                    no_walk = false;
                }
                rev_input_given = true;
            }
        }
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
    if tips.is_empty() && !objects && !read_stdin && !rev_input_given {
        return Ok(usage_error());
    }
    dedup_in_place(&mut tips);

    // 1. Full commit list in date order — the input every later stage refines.
    let mut commits: Vec<ObjectId> = Vec::new();
    let mut parents_of: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
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
        if !hidden.is_empty() {
            platform = platform.with_hidden(hidden.clone());
        }
        for info in platform.all()? {
            let info = info?;
            parents_of.insert(info.id, info.parent_ids.to_vec());
            commits.push(info.id);
        }
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

    // `SYMMETRIC_LEFT` reaches every ancestor of the left tip: `process_parents`
    // ORs it onto each parent it walks through, so membership is reachability.
    let left_tips: Vec<ObjectId> = seeds
        .iter()
        .filter(|s| s.symmetric_left && !s.uninteresting)
        .map(|s| s.id)
        .collect();
    let left = reachable_from(&left_tips, &parents_of);

    // `cherry_pick_list` gives up before computing any patch id when one side of
    // the symmetric difference is empty, which is the only case this port covers.
    if cherry_mark {
        let left_count = commits.iter().filter(|id| left.contains(*id)).count();
        if left_count != 0 && left_count != commits.len() {
            return Ok(fatal(
                "--cherry-mark with commits on both sides is not supported",
            ));
        }
    }

    // `--ancestry-path`: keep only the commits that descend from an excluded tip.
    if ancestry_path {
        let bottoms: Vec<ObjectId> = seeds.iter().filter(|s| s.bottom).map(|s| s.id).collect();
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
    if !pathspecs.is_empty() {
        let mut specs = super::log::PathspecMatcher::new(&repo, &pathspecs)?;
        let mut simplified: Vec<(ObjectId, Vec<ObjectId>)> = Vec::new();
        for id in &commits {
            let parents = parents_of.get(id).map(Vec::as_slice).unwrap_or(&[]);
            let same =
                treesame_parent(&repo, *id, parents, first_parent, &mut specs, &parents_of)?;
            match same {
                None => {}
                Some(parent) => {
                    treesame.insert(*id);
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

    // 2. Reorder, 3. filter by parent count, 4. limit, 5. reverse — in that
    // order, because git sorts the whole list, then drops commits at output
    // time, and only counts what it actually emits against `--max-count`.
    // `prepare_revision_walk()` returns as soon as `revs->no_walk` survived
    // (revision.c:4009), which is *before* both `sort_in_topological_order()` and
    // `init_topo_walk()` — so `--topo-order` and `--date-order` are silently inert
    // under `--no-walk`, and the pending order (or its date sort) stands.
    if order != Order::Date && !no_walk {
        let dates: Option<HashMap<ObjectId, i64>> = (order == Order::DateTopo).then(|| {
            commits
                .iter()
                .map(|id| (*id, commit_date(&repo, *id)))
                .collect()
        });
        commits = topo_sort(&commits, &parents_of, dates.as_ref());
    }

    // `--bisect` replaces the whole list with the one commit `find_bisection`
    // picks, before any output-time filter runs.
    if bisect {
        if !pathspecs.is_empty() {
            // git weighs a TREESAME commit as reaching nothing, and this port has
            // no TREESAME marking during the walk to weigh with.
            return Ok(fatal("--bisect with a pathspec is not supported"));
        }
        commits = find_bisection(&commits, &parents_of, first_parent)
            .into_iter()
            .collect();
    }

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
        author_res: compile_patterns(&author_pats, dialect, ignore_case)?,
        committer_res: compile_patterns(&committer_pats, dialect, ignore_case)?,
        grep_res: compile_patterns(&grep_pats, dialect, ignore_case)?,
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
    // is reported as the nearest ancestor that survived.
    if !pathspecs.is_empty() && show_parents {
        let survivors: HashSet<ObjectId> = commits.iter().copied().collect();
        let rewritten: Vec<(ObjectId, Vec<ObjectId>)> = commits
            .iter()
            .map(|id| {
                (
                    *id,
                    rewrite_parents(id, &survivors, &parents_of, first_parent),
                )
            })
            .collect();
        parents_of.extend(rewritten);
    }

    if let Some(max) = max_count {
        commits.truncate(max);
    }

    // `--boundary`: the parents of the commits the walk returned that were not
    // themselves returned, appended once `get_revision_1()` runs dry. The marking
    // runs over the emission order, so it has to happen before `--reverse` — which
    // `get_revision()` applies to the *whole* sequence, boundary commits included
    // (revision.c:4673-4692), putting them in front and reversing their own order.
    let boundary_commits = if boundary {
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
    let mut count_left = 0usize;
    let mut count_right = 0usize;
    let count_same = 0usize;
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
        pathspecs: object_specs.as_ref(),
    };
    let mut absent: Vec<ObjectId> = Vec::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    if objects && !hidden.is_empty() {
        mark_hidden_objects(&repo, &hidden, first_parent, &mut seen)?;
    }
    // Each entry is the object and the path it was reached through, which
    // `--no-object-names` drops at render time. The tag objects seeded from refs
    // come ahead of any tree, named by the tag's own name field.
    let mut object_lines: Vec<(ObjectId, Vec<u8>)> = Vec::new();
    if objects {
        for (id, name) in &pending_tags {
            if seen.insert(*id) {
                object_lines.push((*id, name.clone()));
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

    for (id, is_boundary) in &emitted {
        if disk_usage {
            match object_disk_size(&repo, *id) {
                Some(n) => disk_total += n,
                None => return Ok(fatal(&format!("unable to get disk usage of {id}"))),
            }
        }
        if count_only && !quiet {
            if left.contains(id) {
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
                out.extend_from_slice(revision_mark(
                    *is_boundary,
                    left.contains(id),
                    left_right,
                    cherry_mark,
                ));
                // `if (revs->abbrev_commit && revs->abbrev)` — both are needed,
                // which is why `--abbrev=8` alone prints the whole id and
                // `--abbrev-commit --no-abbrev` does too.
                if abbrev_commit && abbrev_len != Some(0) {
                    out.extend_from_slice(id.attach(&repo).shorten_or_id().to_string().as_bytes());
                } else {
                    out.extend_from_slice(id.to_string().as_bytes());
                }
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
                let body = rev_list_pretty_body(&repo, &object.into_commit(), p)?;
                if !body.is_empty() {
                    out.extend_from_slice(&body);
                    out.push(hdr_term);
                }
            }
        }
        if objects && in_commit_order && !is_boundary {
            if let Err(code) = collect_commit_objects(
                &repo,
                *id,
                &mut seen,
                &mut object_lines,
                &mut absent,
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

    if objects && !in_commit_order {
        for id in &commits {
            if let Err(code) = collect_commit_objects(
                &repo,
                *id,
                &mut seen,
                &mut object_lines,
                &mut absent,
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
    // `--missing=print` reports what the walk could not find, after the listing.
    for id in &absent {
        writeln!(sink, "?{id}")?;
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

/// git's `get_revision_mark`: the character printed in front of the object name.
///
/// A boundary commit wins over everything, then a patch-equivalent one (which
/// this port never produces, see `--cherry-mark`), then the symmetric side under
/// `--left-right`, and finally `--cherry-mark`'s plain `+`.
fn revision_mark(
    is_boundary: bool,
    is_left: bool,
    left_right: bool,
    cherry_mark: bool,
) -> &'static [u8] {
    if is_boundary {
        b"-"
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
fn seed_ref_set(
    repo: &gix::Repository,
    sel: &super::log::RefSelection,
    negate: bool,
    seeds: &mut Vec<Seed>,
    tags: &mut Vec<(ObjectId, Vec<u8>)>,
) -> Result<(), String> {
    let refs = repo.references().map_err(|e| e.to_string())?;
    let iter = refs.all().map_err(|e| e.to_string())?;
    for reference in iter {
        let reference = reference.map_err(|e| e.to_string())?;
        let full = reference.name().as_bstr().to_string();
        if sel.selects(&full).is_none() {
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
        if let Some(id) = peel_recording_tags(repo, target, tags) {
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
    if sel.head && !sel.excluded("HEAD") {
        if let Ok(head) = repo.head_id() {
            if let Some(id) = peel_recording_tags(repo, head.detach(), tags) {
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
    seeds: &mut Vec<Seed>,
    tags: &mut Vec<(ObjectId, Vec<u8>)>,
) -> Result<(), String> {
    let unknown = |s: &str| unresolvable(repo, s);
    if let Some(rest) = spec.strip_prefix('^') {
        let id = resolve(repo, rest, tags).ok_or_else(|| unknown(spec))?;
        seeds.push(Seed {
            id,
            uninteresting: !negate,
            symmetric_left: false,
            bottom: !negate,
        });
        return Ok(());
    }
    // `add_parents_only()`: `<rev>^@` pends the parents alone and returns, while
    // `<rev>^!` pends them with `flags ^ (UNINTERESTING | BOTTOM)` and then falls
    // through so the commit itself is pended after them.
    if let Some(base) = spec.strip_suffix("^@").or_else(|| spec.strip_suffix("^!")) {
        let excludes_parents = spec.ends_with("^!");
        let id = resolve(repo, base, tags).ok_or_else(|| unknown(spec))?;
        let parents: Vec<ObjectId> = match repo.find_object(id).and_then(|o| o.peel_tags_to_end()) {
            Ok(o) => match o.try_into_commit() {
                Ok(c) => c.parent_ids().map(|p| p.detach()).collect(),
                // `if (it->type != OBJ_COMMIT) return 0;` — a non-commit makes
                // `add_parents_only()` fail, and the spelling falls back to being
                // read as an ordinary revision.
                Err(_) => return seed_plain(repo, spec, negate, seeds, tags),
            },
            Err(_) => return Err(unknown(spec)),
        };
        let parents_negated = if excludes_parents { !negate } else { negate };
        for p in parents {
            seeds.push(Seed {
                id: p,
                uninteresting: parents_negated,
                symmetric_left: false,
                bottom: parents_negated,
            });
        }
        if excludes_parents {
            seeds.push(Seed {
                id,
                uninteresting: negate,
                symmetric_left: false,
                bottom: negate,
            });
        }
        return Ok(());
    }
    if let Some((l, r)) = spec.split_once("...") {
        let left_spec = if l.is_empty() { "HEAD" } else { l };
        let right_spec = if r.is_empty() { "HEAD" } else { r };
        let left = resolve(repo, left_spec, tags).ok_or_else(|| unknown(spec))?;
        let right = resolve(repo, right_spec, tags).ok_or_else(|| unknown(spec))?;
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
        let left = resolve(repo, left_spec, tags).ok_or_else(|| unknown(spec))?;
        let right = resolve(repo, right_spec, tags).ok_or_else(|| unknown(spec))?;
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
    seed_plain(repo, spec, negate, seeds, tags)
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
    seeds: &mut Vec<Seed>,
    tags: &mut Vec<(ObjectId, Vec<u8>)>,
) -> Result<(), String> {
    let id = resolve(repo, spec, tags).ok_or_else(|| unresolvable(repo, spec))?;
    seeds.push(Seed {
        id,
        uninteresting: negate,
        symmetric_left: false,
        bottom: negate,
    });
    Ok(())
}

/// git's `--bisect` pseudo-option: seed from the bisect refs.
///
/// `refs/bisect/<term_bad>*` are the tips and `refs/bisect/<term_good>*` are
/// excluded, with the terms coming from `BISECT_TERMS` when a session renamed
/// them (`read_bisect_terms`, defaults `bad`/`good`). Both are prefix matches,
/// which is how the per-commit `good-<oid>` refs are picked up.
fn seed_bisect_refs(
    repo: &gix::Repository,
    negate: bool,
    seeds: &mut Vec<Seed>,
    tags: &mut Vec<(ObjectId, Vec<u8>)>,
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
        if let Some(id) = peel_recording_tags(repo, target, tags) {
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
/// git also skips TREESAME commits, which only exist under a pathspec-limited
/// walk; `--bisect` with a pathspec is rejected rather than weighed wrongly.
fn find_bisection(
    commits: &[ObjectId],
    parents_of: &HashMap<ObjectId, Vec<ObjectId>>,
    first_parent_only: bool,
) -> Option<ObjectId> {
    // git reverses the list while counting, so the oldest commit comes first.
    let list: Vec<ObjectId> = commits.iter().rev().copied().collect();
    let nr = list.len() as i64;
    if nr == 0 {
        return None;
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
            0 => {
                weights[n] = 1;
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
        weights[n] = count_distance(list[n], parents_of, &slot, &mut visited, first_parent_only);
        if approx_halfway(weights[n]) {
            return Some(list[n]);
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
            weights[n] = w + 1;
            counted += 1;
            progressed = true;
            if approx_halfway(weights[n]) {
                return Some(list[n]);
            }
        }
        if !progressed {
            break;
        }
    }

    // `best_bisection`: the commit whose smaller side is largest.
    let mut best = list[0];
    let mut best_distance = -1i64;
    for (n, id) in list.iter().enumerate() {
        let distance = weights[n].min(nr - weights[n]);
        if distance > best_distance {
            best = *id;
            best_distance = distance;
        }
    }
    Some(best)
}

/// git's `count_distance`: how many listed commits `start` reaches, itself
/// included, counting each at most once across the whole walk.
fn count_distance(
    start: ObjectId,
    parents_of: &HashMap<ObjectId, Vec<ObjectId>>,
    slot: &HashMap<ObjectId, usize>,
    visited: &mut HashSet<ObjectId>,
    first_parent_only: bool,
) -> i64 {
    let mut nr = 0;
    let mut cur = Some(start);
    while let Some(id) = cur {
        if !slot.contains_key(&id) || !visited.insert(id) {
            break;
        }
        nr += 1;
        let parents = parents_of.get(&id).map(Vec::as_slice).unwrap_or(&[]);
        cur = parents.first().copied();
        if first_parent_only {
            continue;
        }
        // A merge's extra parents are separate strands, counted recursively.
        for extra in parents.iter().skip(1) {
            nr += count_distance(*extra, parents_of, slot, visited, first_parent_only);
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
fn resolve(
    repo: &gix::Repository,
    spec: &str,
    tags: &mut Vec<(ObjectId, Vec<u8>)>,
) -> Option<ObjectId> {
    // Every endpoint reaches `repo_get_oid()`, so a full-length hex that is also a
    // refname warns here — once per endpoint, which is why `a..b` warns twice.
    crate::objname::warn_ambiguous_refname(repo, spec);
    let id = repo.rev_parse_single(spec).ok()?.detach();
    peel_recording_tags(repo, id, tags)
}

/// Peel `id` down to a commit, pushing every tag object passed through onto
/// `tags` under its own name — which is what `--objects` reports for them.
fn peel_recording_tags(
    repo: &gix::Repository,
    id: ObjectId,
    tags: &mut Vec<(ObjectId, Vec<u8>)>,
) -> Option<ObjectId> {
    let mut id = id;
    loop {
        let object = repo.find_object(id).ok()?;
        let kind = object.kind;
        match kind {
            gix::object::Kind::Commit => return Some(id),
            gix::object::Kind::Tag => {
                let tag = object.into_tag();
                let tag_id = tag.id;
                let (name, target) = {
                    let decoded = tag.decode().ok()?;
                    (decoded.name.to_vec(), decoded.target())
                };
                tags.push((tag_id, name));
                id = target;
            }
            _ => return None,
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
    absent: &mut Vec<ObjectId>,
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
    if !matches!(walk.filter, Some(Filter::TreeDepth(0))) {
        lines.push((tree, Vec::new()));
    }
    walk_tree(repo, tree, &[], 1, seen, lines, absent, walk)
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
    for parent in considered {
        let parent_tree = commit_tree(repo, *parent);
        if diff_touches_path(repo, parent_tree, tree, specs)? {
            continue;
        }
        // A parent outside the walk is UNINTERESTING, and `relevant_commit`
        // refuses to simplify onto one, so keep looking for a relevant match.
        if walked.contains_key(parent) {
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
    absent: &mut Vec<ObjectId>,
    walk: &ObjectWalk<'_>,
) -> Result<Result<(), ExitCode>> {
    // Nothing at this depth, or under it, survives the tree filter.
    if matches!(walk.filter, Some(Filter::TreeDepth(max)) if depth >= max) {
        return Ok(Ok(()));
    }
    let Some(object) = tree_object(repo, tree) else {
        if let Some(code) = note_missing(tree, absent, walk.missing) {
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
            lines.push((id, path.clone()));
            if let Err(code) = walk_tree(repo, id, &path, depth + 1, seen, lines, absent, walk)? {
                return Ok(Err(code));
            }
            continue;
        }
        match blob_filtered(repo, id, absent, walk)? {
            Ok(true) => continue,
            Ok(false) => {}
            Err(code) => return Ok(Err(code)),
        }
        lines.push((id, path));
    }
    Ok(Ok(()))
}

/// git's `finish_object__ma`: record or reject an object the repository lacks.
/// `Some(code)` means the walk must stop with that exit code.
fn note_missing(id: ObjectId, absent: &mut Vec<ObjectId>, missing: Missing) -> Option<ExitCode> {
    match missing {
        Missing::Error => Some(fatal(&format!("missing object '{id}'"))),
        Missing::AllowAny => None,
        Missing::Print => {
            if !absent.contains(&id) {
                absent.push(id);
            }
            None
        }
    }
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
    absent: &mut Vec<ObjectId>,
    walk: &ObjectWalk<'_>,
) -> Result<Result<bool, ExitCode>> {
    let header = repo.find_header(id).ok();
    let Some(header) = header else {
        if let Some(code) = note_missing(id, absent, walk.missing) {
            return Ok(Err(code));
        }
        // A missing object is skipped rather than listed.
        return Ok(Ok(true));
    };
    Ok(Ok(match walk.filter {
        Some(Filter::BlobNone) => true,
        Some(Filter::BlobLimit(limit)) => header.size() >= limit,
        _ => false,
    }))
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
        let loose = dir.join(&hex[..2]).join(&hex[2..]);
        if let Ok(md) = std::fs::metadata(&loose) {
            return Some(md.len());
        }
        if let Some(size) = packed_entry_size(&dir.join("pack"), id) {
            return Some(size);
        }
    }
    None
}

/// The length of `id`'s entry inside a pack: the gap to the next entry in
/// offset order, which is what git's reverse index computes.
fn packed_entry_size(pack_dir: &std::path::Path, id: ObjectId) -> Option<u64> {
    let entries = std::fs::read_dir(pack_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("idx") {
            continue;
        }
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
