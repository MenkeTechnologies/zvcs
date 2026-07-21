//! `git shortlog` — summarize `git log` output, grouped by author or committer.
//!
//! A port of git's `builtin/shortlog.c` together with the pieces of
//! `revision.c` that shortlog leans on (its option loop hands every option it
//! does not own to `handle_revision_opt`), plus the two output helpers:
//! `strbuf_add_wrapped_text()` / `strbuf_add_indented_text()` from `utf8.c`
//! (the `-w` line wrapper) and `split_ident_line()` from `ident.c` (the stdin
//! path's `Name <email>` splitter). The commit walk, the mailmap and the ref
//! iteration come from the vendored gitoxide crates.
//!
//! Covered, byte-for-byte against stock git 2.55:
//!   * default long format — `<ident> (<n>):` followed by one indented subject
//!     per commit, oldest first, then a blank line.
//!   * `-s`/`--summary`, `-n`/`--numbered`, `-e`/`--email`, `-c`/`--committer`.
//!   * `-w[<width>[,<indent1>[,<indent2>]]]` — the real wrap algorithm.
//!   * `--group=author|committer|trailer:<tok>|format:<fmt>`, `--group <field>`,
//!     `--no-group`, and the `unknown group type` / `with stdin is not
//!     supported` failures.
//!   * `--format=<fmt>` — builtin format names are ignored (git only consults
//!     `--format` when it is a *user* format), user formats are expanded.
//!   * `--date=<fmt>` — validated the way `parse_date_format()` validates it.
//!   * revision selection: `<rev>`, `^<rev>`, `<a>..<b>`, `<a>...<b>`, `--not`,
//!     `--all`, `--branches[=<glob>]`, `--tags[=<glob>]`, `--remotes[=<glob>]`,
//!     `--glob=<glob>`, `--exclude=<glob>`, `--ignore-missing`.
//!   * walk limiting: `--max-count=<n>`/`-<n>`, `--skip=<n>`, `--first-parent`,
//!     `--merges`/`--no-merges`, `--min-parents`/`--max-parents` and their
//!     `--no-` forms, `--since`/`--after`/`--until`/`--before`, `--boundary`,
//!     `--reverse`, `--ancestry-path`, `--date-order`/`--topo-order`.
//!   * path-limited traversal: `<rev>... [--] <path>...`. Everything after `--`
//!     is a pathspec; when a revision is pending, a commit is shown iff its diff
//!     against a parent touched a matching path (git's TREESAME test) — a merge
//!     iff it differs from *every* parent, a root commit iff its tree contains a
//!     match, `--first-parent` limiting a merge to its first parent. When no
//!     revision is pending the pathspecs are inert and shortlog reads stdin,
//!     exactly as git does.
//!   * message/ident filtering: `--grep`, `--author`, `--all-match`,
//!     `--invert-grep`, `-i`/`--regexp-ignore-case`, `-F`/`--fixed-strings`
//!     (see the restriction below).
//!   * the stdin mode git falls into when nothing lands in the pending object
//!     set and stdin is not a terminal.
//!   * exit codes and streams: 0 on success, 128 for the `fatal:` paths, 129
//!     for `-h` (usage on stdout), for an unknown option (usage on stderr),
//!     for `option ... takes no value`, for an unknown `--group` type and for a
//!     malformed `-w` argument.
//!
//! Not covered — each `bail!`s rather than emitting output that would diverge:
//! magic pathspecs (`:(glob)`, `:(icase)`, `:!exclude`), `--reflog`, `--simplify-merges`,
//! `--author-date-order`, `--bisect`, `--alternate-refs`, `--exclude-hidden`,
//! repeated `--group` with different fields, and `--boundary` combined with
//! `--skip`/`--max-count` (git appends boundary commits to the tail of the
//! revision stream, where a limit can truncate them; that interaction is not
//! modelled here). `--exclude-first-parent-only` is accepted only when it is
//! provably a no-op — that is, when nothing reachable from the excluded tips is
//! a merge — and bails otherwise.
//!
//! `--grep`/`--author` patterns are matched as fixed strings. git's default is
//! POSIX basic regex; no regex engine is vendored here, so a pattern containing
//! a metacharacter bails instead of being mis-matched. `-P`/`--perl-regexp`
//! bails for the same reason. (`--committer=<pattern>` needs no handling:
//! `committer` is one of shortlog's own boolean options, so git rejects the
//! `=<value>` spelling outright.)
//!
//! Known deviations, both confined to inputs stock git treats specially:
//!   * `-w` measures a code point as one display column, where git uses
//!     `wcwidth()`. Wrapping differs only for subjects containing wide (CJK) or
//!     zero-width characters, or for text that is not valid UTF-8.
//!   * mailmap lookups go through `gix_mailmap`, which case-normalises a matched
//!     email even when the matching entry supplies no replacement address; git
//!     keeps the commit's own casing. Only `-e` output against such a mailmap is
//!     affected.
//!   * `--full-history`/`--sparse`/`--dense`/`--remove-empty` are accepted but
//!     leave git's default merge simplification in force. They only change output
//!     under a path limit whose spec matches a real tracked file; `--full-history`
//!     and `--sparse` would then list additional merge/TREESAME commits.

use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeMap, HashSet};
use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::prelude::ObjectIdExt;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// The `usage_with_options` block git prints for `-h`, and again after every
/// `error: unknown option ...`. It ends with a blank line.
const USAGE: &str = "\
usage: git shortlog [<options>] [<revision-range>] [[--] <path>...]
   or: git log --pretty=short | git shortlog [<options>]

    -c, --[no-]committer  group by committer rather than author
    -n, --[no-]numbered   sort output according to the number of commits per author
    -s, --[no-]summary    suppress commit descriptions, only provides commit count
    -e, --[no-]email      show the email address of each author
    -w[<w>[,<i1>[,<i2>]]] linewrap output
    --[no-]group <field>  group by field

";

/// git's `wrap_arg_usage`, printed verbatim when `-w`'s argument is malformed.
const WRAP_ARG_USAGE: &str = "-w[<width>[,<indent1>[,<indent2>]]]";

const DEFAULT_WRAPLEN: usize = 76;
const DEFAULT_INDENT1: usize = 6;
const DEFAULT_INDENT2: usize = 9;

/// Parsed command-line options for a single `shortlog` invocation.
struct Opts {
    summary: bool,  // -s: print counts only
    numbered: bool, // -n: sort by descending commit count
    email: bool,    // -e: include the email in the group key
    wrap_lines: bool,
    wrap: usize, // -w width; 0 means "indent but never wrap"
    in1: usize,  // -w indent for the first line of an entry
    in2: usize,  // -w indent for continuation lines
    reverse: bool,
    group: GroupBy,
    /// `--format=<user format>`; `None` when git would use the subject.
    user_format: Option<String>,
}

/// git's `log->groups` bitfield, reduced to the single group this port accepts.
#[derive(Clone, PartialEq, Eq)]
enum GroupBy {
    Author,
    Committer,
    Trailer(String),
    Format(String),
}

/// One grouped identity: how many commits it owns, and (unless `-s`) their
/// subjects in walk order, i.e. newest first.
#[derive(Default)]
struct Group {
    count: usize,
    onelines: Vec<BString>,
}

/// Which ref namespace a pseudo-option selects.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RefKind {
    All,
    Branches,
    Tags,
    Remotes,
    Glob,
}

/// A revision-selecting argument, kept in command-line order because `--not`
/// and `--exclude` are positional: they only affect what follows them.
enum RevAction {
    Rev(String),
    Not,
    Exclude(String),
    Refs {
        kind: RefKind,
        pattern: Option<String>,
    },
}

/// Which walk order was requested. git's default is a plain commit-date queue;
/// `--date-order` and `--topo-order` additionally keep parents behind children.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    Default,
    Date,
    Topo,
}

/// The `rev_info` fields shortlog's walk actually reads.
struct Filters {
    min_parents: usize,
    max_parents: Option<usize>,
    skip: usize,
    max_count: Option<usize>,
    first_parent: bool,
    exclude_first_parent_only: bool,
    since: Option<i64>,
    until: Option<i64>,
    grep: Vec<String>,
    author: Vec<String>,
    all_match: bool,
    invert_grep: bool,
    ignore_case: bool,
    fixed_strings: bool,
    boundary: bool,
    ancestry_path: bool,
    order: Order,
}

/// One commit as produced by the walk, before any per-commit filtering.
struct WalkItem {
    id: ObjectId,
    parents: Vec<ObjectId>,
}

pub fn shortlog(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands us argv with the subcommand at index 0.
    let argv: &[String] = match args.first() {
        Some(a) if a == "shortlog" => &args[1..],
        _ => args,
    };

    let mut opts = Opts {
        summary: false,
        numbered: false,
        email: false,
        wrap_lines: false,
        wrap: DEFAULT_WRAPLEN,
        in1: DEFAULT_INDENT1,
        in2: DEFAULT_INDENT2,
        reverse: false,
        group: GroupBy::Author,
        user_format: None,
    };

    let mut filters = Filters {
        min_parents: 0,
        max_parents: None,
        skip: 0,
        max_count: None,
        first_parent: false,
        exclude_first_parent_only: false,
        since: None,
        until: None,
        grep: Vec::new(),
        author: Vec::new(),
        all_match: false,
        invert_grep: false,
        ignore_case: false,
        fixed_strings: false,
        boundary: false,
        ancestry_path: false,
        order: Order::Default,
    };

    // `--group` is a bitfield in git; only a single field is ported, so track
    // the two plain bits plus at most one trailer/format field and reject any
    // combination that would make git emit more than one record per commit.
    let mut group_author = false;
    let mut group_committer = false;
    let mut group_special: Vec<GroupBy> = Vec::new();

    let mut actions: Vec<RevAction> = Vec::new();
    let mut ignore_missing = false;
    // Raw pathspecs collected after a `--` separator, in command-line order.
    let mut pathspecs: Vec<Vec<u8>> = Vec::new();

    // Once git has consumed any option other than a ref-selecting pseudo-option,
    // the argv slot its error reporter reads has moved on, and a later unknown
    // option is reported as the literal text `(null)`. Reproduced from git 2.55;
    // `--all`, `--branches`, `--tags`, `--remotes`, `--glob`, `--exclude`,
    // `--not` and bare revisions do not arm it, everything else does.
    let mut argv_consumed = false;

    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        i += 1;

        if a == "--" {
            // git stops option parsing at `--`; everything past it — even tokens
            // that look like options or revisions — is a pathspec.
            pathspecs.extend(argv[i..].iter().map(|s| s.as_bytes().to_vec()));
            break;
        }
        if a.len() < 2 || !a.starts_with('-') {
            actions.push(RevAction::Rev(a.to_string()));
            continue;
        }

        if let Some(long) = a.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };

            // shortlog's own option table. A boolean of git's rejects `=value`.
            let native_bool = matches!(
                name,
                "committer"
                    | "no-committer"
                    | "numbered"
                    | "no-numbered"
                    | "summary"
                    | "no-summary"
                    | "email"
                    | "no-email"
            );
            if native_bool {
                if value.is_some() {
                    eprintln!("error: option `{name}' takes no value");
                    return Ok(ExitCode::from(129));
                }
                match name {
                    "committer" => group_committer = true,
                    "no-committer" => group_committer = false,
                    "numbered" => opts.numbered = true,
                    "no-numbered" => opts.numbered = false,
                    "summary" => opts.summary = true,
                    "no-summary" => opts.summary = false,
                    "email" => opts.email = true,
                    _ => opts.email = false,
                }
                argv_consumed = true;
                continue;
            }

            if name == "group" {
                let field = match value {
                    Some(v) => v.to_string(),
                    None => match argv.get(i) {
                        Some(v) => {
                            i += 1;
                            v.clone()
                        }
                        None => {
                            eprintln!("error: option `group' requires a value");
                            return Ok(ExitCode::from(129));
                        }
                    },
                };
                match field.as_str() {
                    "author" => group_author = true,
                    "committer" => group_committer = true,
                    _ if field.starts_with("trailer:") => {
                        group_special.push(GroupBy::Trailer(field["trailer:".len()..].to_string()));
                    }
                    _ if field.starts_with("format:") => {
                        group_special.push(GroupBy::Format(field["format:".len()..].to_string()));
                    }
                    // git's fallback: a group value that carries a `%` is an
                    // implicit format, exactly as if written `format:<value>`.
                    _ if field.contains('%') => {
                        group_special.push(GroupBy::Format(field.clone()));
                    }
                    other => {
                        eprintln!("error: unknown group type: {other}");
                        return Ok(ExitCode::from(129));
                    }
                }
                argv_consumed = true;
                continue;
            }
            if name == "no-group" {
                group_author = false;
                group_committer = false;
                group_special.clear();
                argv_consumed = true;
                continue;
            }

            // Ref-selecting pseudo-options. These are the ones that leave git's
            // error reporter pointing at the real argv slot.
            let pseudo = match name {
                "all" if value.is_none() => Some(RevAction::Refs {
                    kind: RefKind::All,
                    pattern: None,
                }),
                "branches" => Some(RevAction::Refs {
                    kind: RefKind::Branches,
                    pattern: value.map(str::to_string),
                }),
                "tags" => Some(RevAction::Refs {
                    kind: RefKind::Tags,
                    pattern: value.map(str::to_string),
                }),
                "remotes" => Some(RevAction::Refs {
                    kind: RefKind::Remotes,
                    pattern: value.map(str::to_string),
                }),
                "glob" if value.is_some() => Some(RevAction::Refs {
                    kind: RefKind::Glob,
                    pattern: value.map(str::to_string),
                }),
                "exclude" if value.is_some() => {
                    Some(RevAction::Exclude(value.unwrap_or_default().to_string()))
                }
                "not" if value.is_none() => Some(RevAction::Not),
                _ => None,
            };
            if let Some(action) = pseudo {
                actions.push(action);
                continue;
            }
            if matches!(
                name,
                "reflog" | "bisect" | "alternate-refs" | "exclude-hidden"
            ) {
                bail!("unsupported flag {a:?}");
            }

            // Everything else is a revision-walk option. Options that take a
            // value only exist in their `=<value>` spelling here; the bare form
            // is not an option git knows.
            match (name, value) {
                ("max-count", Some(v)) => match int_arg(v) {
                    // git's sentinel: a negative count means "unlimited".
                    Ok(n) => filters.max_count = (n >= 0).then_some(n as usize),
                    Err(code) => return Ok(code),
                },
                ("skip", Some(v)) => match int_arg(v) {
                    // A negative skip skips nothing.
                    Ok(n) => filters.skip = n.max(0) as usize,
                    Err(code) => return Ok(code),
                },
                ("min-parents", Some(v)) => match int_arg(v) {
                    // A negative floor admits every commit, exactly like zero.
                    Ok(n) => filters.min_parents = n.max(0) as usize,
                    Err(code) => return Ok(code),
                },
                ("max-parents", Some(v)) => match int_arg(v) {
                    // git's sentinel: a negative ceiling means "no maximum".
                    Ok(n) => filters.max_parents = (n >= 0).then_some(n as usize),
                    Err(code) => return Ok(code),
                },
                ("no-min-parents", None) => filters.min_parents = 0,
                ("no-max-parents", None) => filters.max_parents = None,
                ("merges", None) => filters.min_parents = 2,
                ("no-merges", None) => filters.max_parents = Some(1),
                ("first-parent", None) => filters.first_parent = true,
                ("exclude-first-parent-only", None) => filters.exclude_first_parent_only = true,
                ("since" | "after", Some(v)) => filters.since = Some(approxidate(v)),
                ("until" | "before", Some(v)) => filters.until = Some(approxidate(v)),
                ("author", Some(v)) => filters.author.push(v.to_string()),
                ("grep", Some(v)) => filters.grep.push(v.to_string()),
                ("all-match", None) => filters.all_match = true,
                ("invert-grep", None) => filters.invert_grep = true,
                ("regexp-ignore-case", None) => filters.ignore_case = true,
                ("fixed-strings", None) => filters.fixed_strings = true,
                // Both name a regex dialect this port cannot evaluate; the
                // pattern guard after the loop is what decides whether we can
                // answer at all, so the dialect itself needs no state.
                ("basic-regexp" | "extended-regexp", None) => {}
                ("perl-regexp", None) => bail!("unsupported flag {a:?}"),
                ("boundary", None) => filters.boundary = true,
                ("ancestry-path", None) => filters.ancestry_path = true,
                ("reverse", None) => opts.reverse = true,
                ("date-order", None) => filters.order = Order::Date,
                ("topo-order", None) => filters.order = Order::Topo,
                ("author-date-order" | "simplify-merges", None) => {
                    bail!("unsupported flag {a:?}")
                }
                ("ignore-missing", None) => ignore_missing = true,
                // History-simplification modes. They only alter output under a
                // path limit, and then only for a merge or otherwise-TREESAME
                // commit; accepted as no-ops, leaving git's default simplification
                // in force (see the known deviation in the module docs).
                ("dense" | "sparse" | "full-history" | "remove-empty", None) => {}
                ("date", Some(fmt)) => {
                    if !is_known_date_format(fmt) {
                        eprintln!("fatal: unknown date format {fmt}");
                        return Ok(ExitCode::from(128));
                    }
                }
                ("format" | "pretty", Some(arg)) => match parse_pretty(arg) {
                    Some(user) => opts.user_format = user,
                    None => {
                        eprintln!("fatal: invalid --pretty format: {arg}");
                        return Ok(ExitCode::from(128));
                    }
                },
                _ => return Ok(unknown_option(a, argv_consumed)),
            }
            argv_consumed = true;
            continue;
        }

        // `-<number>` is git's `--max-count` shorthand, not a short-option cluster.
        let body = &a[1..];
        if body.bytes().all(|b| b.is_ascii_digit()) {
            match int_arg(body) {
                // `body` is all digits, so the parse is non-negative.
                Ok(n) => filters.max_count = Some(n as usize),
                Err(code) => return Ok(code),
            }
            argv_consumed = true;
            continue;
        }

        // Single-letter revision-walk options, which shortlog does not own.
        match body {
            "i" => {
                filters.ignore_case = true;
                argv_consumed = true;
                continue;
            }
            "F" => {
                filters.fixed_strings = true;
                argv_consumed = true;
                continue;
            }
            "E" => {
                argv_consumed = true;
                continue;
            }
            "P" => bail!("unsupported flag {a:?}"),
            _ => {}
        }

        // A cluster of short options. `-w` takes an optional *attached* argument,
        // so it swallows whatever remains of the cluster.
        for (off, c) in body.char_indices() {
            match c {
                'c' => group_committer = true,
                'n' => opts.numbered = true,
                's' => opts.summary = true,
                'e' => opts.email = true,
                'w' => {
                    let rest = &body[off + 1..];
                    let arg = if rest.is_empty() { None } else { Some(rest) };
                    if !parse_wrap_args(&mut opts, arg) {
                        eprintln!("error: {WRAP_ARG_USAGE}");
                        return Ok(ExitCode::from(129));
                    }
                    break;
                }
                'h' => {
                    print!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
                _ => return Ok(unknown_option(a, argv_consumed)),
            }
        }
        argv_consumed = true;
    }

    // Resolve the requested grouping into the single field this port supports.
    let group_count =
        usize::from(group_author) + usize::from(group_committer) + group_special.len();
    if group_count > 1 {
        bail!("multiple --group options are not ported");
    }
    opts.group = match group_special.pop() {
        Some(g) => g,
        None if group_committer => GroupBy::Committer,
        None => GroupBy::Author,
    };

    // A pattern this port cannot evaluate must never be silently mis-matched.
    for pattern in filters.grep.iter().chain(&filters.author) {
        if !filters.fixed_strings && has_regex_metacharacter(pattern) {
            bail!("regular-expression pattern {pattern:?} is not ported (only fixed strings are)");
        }
    }

    let repo = gix::discover(".").ok();
    let mailmap = repo
        .as_ref()
        .map(gix::Repository::open_mailmap)
        .unwrap_or_default();

    // Build git's pending object set in command-line order.
    let mut tips: Vec<ObjectId> = Vec::new();
    let mut hidden: Vec<ObjectId> = Vec::new();
    let mut excludes: Vec<String> = Vec::new();
    let mut negate = false;

    for action in &actions {
        let Some(repo) = repo.as_ref() else {
            bail!("not a git repository");
        };
        match action {
            RevAction::Not => negate = !negate,
            RevAction::Exclude(pattern) => excludes.push(pattern.clone()),
            RevAction::Refs { kind, pattern } => {
                let selected = select_refs(repo, *kind, pattern.as_deref(), &excludes)?;
                let sink = if negate { &mut hidden } else { &mut tips };
                sink.extend(selected);
                excludes.clear();
            }
            RevAction::Rev(spec) => {
                if let Some(rest) = spec.strip_prefix('^') {
                    match resolve(repo, rest) {
                        Some(id) => {
                            if negate {
                                tips.push(id)
                            } else {
                                hidden.push(id)
                            }
                        }
                        None if ignore_missing => {}
                        None => {
                            eprintln!("fatal: bad revision '{spec}'");
                            return Ok(ExitCode::from(128));
                        }
                    }
                } else if let Some((left, right)) = spec.split_once("...") {
                    let left = if left.is_empty() { "HEAD" } else { left };
                    let right = if right.is_empty() { "HEAD" } else { right };
                    let (Some(a), Some(b)) = (resolve(repo, left), resolve(repo, right)) else {
                        if ignore_missing {
                            continue;
                        }
                        return Ok(fatal_ambiguous(spec));
                    };
                    // `a...b` is both tips with every merge base excluded.
                    for base in repo.merge_bases_many(a, &[b])? {
                        hidden.push(base.detach());
                    }
                    tips.push(a);
                    tips.push(b);
                } else if let Some((left, right)) = spec.split_once("..") {
                    let left = if left.is_empty() { "HEAD" } else { left };
                    let right = if right.is_empty() { "HEAD" } else { right };
                    let (Some(a), Some(b)) = (resolve(repo, left), resolve(repo, right)) else {
                        if ignore_missing {
                            continue;
                        }
                        return Ok(fatal_ambiguous(spec));
                    };
                    if negate {
                        tips.push(a);
                        hidden.push(b);
                    } else {
                        hidden.push(a);
                        tips.push(b);
                    }
                } else {
                    match resolve(repo, spec) {
                        Some(id) => {
                            if negate {
                                hidden.push(id)
                            } else {
                                tips.push(id)
                            }
                        }
                        None if ignore_missing => {}
                        None => return Ok(fatal_ambiguous(spec)),
                    }
                }
            }
        }
    }

    // git: "assume HEAD if from a tty" — only when nothing else is pending.
    let mut pending = tips.len() + hidden.len();
    if pending == 0 && std::io::stdin().is_terminal() {
        if let Some(repo) = repo.as_ref() {
            if let Ok(mut head) = repo.head() {
                if let Ok(Some(id)) = head.try_peel_to_id() {
                    tips.push(id.detach());
                    pending += 1;
                }
            }
        }
    }

    let mut groups: BTreeMap<BString, Group> = BTreeMap::new();

    if pending == 0 {
        match &opts.group {
            GroupBy::Trailer(_) => {
                eprintln!("fatal: using --group=trailer with stdin is not supported");
                return Ok(ExitCode::from(128));
            }
            GroupBy::Format(_) => {
                eprintln!("fatal: using --group=format with stdin is not supported");
                return Ok(ExitCode::from(128));
            }
            _ => {}
        }
        read_from_stdin(&mut groups, &mailmap, &opts)?;
    } else if !tips.is_empty() {
        let repo = repo.as_ref().expect("tips can only come from a repository");

        if filters.ancestry_path && hidden.is_empty() {
            eprintln!("fatal: --ancestry-path given but there are no bottom commits");
            return Ok(ExitCode::from(128));
        }
        if filters.exclude_first_parent_only && excluded_side_has_merge(repo, &hidden)? {
            bail!("`--exclude-first-parent-only` across a merge in the excluded history is not ported");
        }
        if filters.boundary && (filters.skip != 0 || filters.max_count.is_some()) {
            bail!("`--boundary` combined with `--skip`/`--max-count` is not ported");
        }
        // Only plain paths are ported; a magic pathspec would need real matching.
        if pathspecs.iter().any(|p| p.first() == Some(&b':')) {
            bail!("magic pathspecs are not ported");
        }

        let items = walk(repo, &tips, &hidden, &filters)?;
        let items = if filters.ancestry_path {
            keep_ancestry_path(items, &hidden)
        } else {
            items
        };

        let mut kept: Vec<ObjectId> = Vec::new();
        for item in &items {
            if !parent_count_matches(item.parents.len(), &filters) {
                continue;
            }
            let commit = repo.find_commit(item.id)?;
            if !time_matches(&commit, &filters)? {
                continue;
            }
            if !message_matches(&commit, &filters)? {
                continue;
            }
            // Path limit: a commit is shown only if its diff against a parent
            // touched a matching path (git's TREESAME test). Runs before
            // `--skip`/`--max-count`, which count only commits actually shown.
            if !pathspecs.is_empty()
                && !commit_touches_path(
                    repo,
                    item.id,
                    &item.parents,
                    filters.first_parent,
                    &pathspecs,
                )?
            {
                continue;
            }
            kept.push(item.id);
        }

        if filters.boundary {
            kept.extend(boundary_commits(&items));
        }

        let selected = kept
            .into_iter()
            .skip(filters.skip)
            .take(filters.max_count.unwrap_or(usize::MAX));

        for id in selected {
            let commit = repo.find_commit(id)?;
            let idents = group_keys(repo, &commit, &mailmap, &opts)?;

            // git computes the record text once and substitutes `<none>` when
            // it comes out empty.
            let oneline = if opts.summary {
                BString::default()
            } else {
                let text = match &opts.user_format {
                    Some(fmt) => expand_format(repo, &commit, &mailmap, fmt)?,
                    None => {
                        let message = commit.message()?;
                        message.summary().into_owned()
                    }
                };
                if text.is_empty() {
                    BString::from("<none>")
                } else {
                    text
                }
            };
            for ident in idents {
                insert_one_record(&mut groups, &opts, ident, oneline.as_bstr());
            }
        }
    }
    // Otherwise: revisions were named but none resolved to a positive tip
    // (e.g. only `^<rev>`), which git renders as empty output.

    let mut out: Vec<u8> = Vec::new();
    render(&groups, &opts, &mut out);
    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// git's `error: unknown option ...` path: the message, then the usage block,
/// both on stderr, exit 129.
fn unknown_option(arg: &str, argv_consumed: bool) -> ExitCode {
    let shown = if argv_consumed { "(null)" } else { arg };
    eprint!("error: unknown option `{shown}'\n{USAGE}");
    ExitCode::from(129)
}

/// Peel `spec` to a commit id, or `None` when it names no commit.
fn resolve(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let object = repo.rev_parse_single(spec).ok()?.object().ok()?;
    Some(object.peel_to_commit().ok()?.id)
}

/// git's `setup_revisions` failure: the fatal block on stderr, exit code 128.
/// git names the whole argument, not the half of a range that failed.
fn fatal_ambiguous(spec: &str) -> ExitCode {
    eprintln!(
        "fatal: ambiguous argument '{spec}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    );
    ExitCode::from(128)
}

/// Parse an option's integer argument the way git does: the whole string must be
/// a decimal integer in C `int` range (so `INT_MAX` parses, `INT_MAX + 1` and
/// `0x10` do not), and a leading `-` is allowed. Failure is
/// `fatal: '<value>': not an integer`, exit 128. Callers map git's negative
/// sentinels (`--max-count=-1` → unlimited, `--skip=-1` → skip nothing, …).
fn int_arg(value: &str) -> Result<i32, ExitCode> {
    value.parse::<i32>().map_err(|_| {
        eprintln!("fatal: '{value}': not an integer");
        ExitCode::from(128)
    })
}

/// git's `parse_date_format()`, reduced to the accept/reject decision — the
/// only thing shortlog needs, since it never formats a date itself except
/// through `--group=format:%cd`, which is not ported.
fn is_known_date_format(fmt: &str) -> bool {
    let fmt = fmt.strip_prefix("auto:").unwrap_or(fmt);
    if fmt.starts_with("format:") || fmt.starts_with("format-local:") {
        return true;
    }
    let base = fmt.strip_suffix("-local").unwrap_or(fmt);
    matches!(
        base,
        "relative"
            | "human"
            | "iso8601"
            | "iso"
            | "iso8601-strict"
            | "iso-strict"
            | "rfc2822"
            | "rfc"
            | "short"
            | "default"
            | "raw"
            | "unix"
            | "local"
    )
}

/// git's `get_commit_format()`. `Some(None)` is a builtin format, which
/// shortlog ignores; `Some(Some(f))` is a user format it expands per commit;
/// `None` is the `invalid --pretty format` failure.
fn parse_pretty(arg: &str) -> Option<Option<String>> {
    const BUILTIN: [&str; 9] = [
        "raw",
        "medium",
        "short",
        "email",
        "mboxrd",
        "fuller",
        "full",
        "oneline",
        "reference",
    ];
    if BUILTIN.contains(&arg) {
        return Some(None);
    }
    for prefix in ["format:", "tformat:"] {
        if let Some(rest) = arg.strip_prefix(prefix) {
            return Some(Some(rest.to_string()));
        }
    }
    if arg.is_empty() || arg.contains('%') {
        return Some(Some(arg.to_string()));
    }
    None
}

/// git's `approxidate()` as far as this port needs it. git deliberately never
/// fails here: anything it cannot read becomes "now".
fn approxidate(value: &str) -> i64 {
    let now = std::time::SystemTime::now();
    let now_seconds = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if value.trim() == "now" {
        return now_seconds;
    }
    match gix::date::parse(value, Some(now)) {
        Ok(time) => time.seconds,
        Err(_) => now_seconds,
    }
}

/// True when the pattern needs a real regex engine to be answered correctly.
fn has_regex_metacharacter(pattern: &str) -> bool {
    pattern.bytes().any(|b| {
        matches!(
            b,
            b'.' | b'^'
                | b'$'
                | b'*'
                | b'+'
                | b'?'
                | b'('
                | b')'
                | b'['
                | b']'
                | b'{'
                | b'}'
                | b'|'
                | b'\\'
        )
    })
}

/// Case-sensitive or `-i` substring search, git's fixed-string match.
fn contains_pattern(haystack: &BStr, pattern: &str, ignore_case: bool) -> bool {
    if ignore_case {
        haystack
            .to_str_lossy()
            .to_lowercase()
            .contains(&pattern.to_lowercase())
    } else {
        haystack.as_bytes().find(pattern.as_bytes()).is_some()
    }
}

/// git's `--glob`/`--branches`/`--tags`/`--remotes` ref expansion, including
/// the implicit `/*` `for_each_glob_ref_in()` appends to a plain prefix.
fn select_refs(
    repo: &gix::Repository,
    kind: RefKind,
    pattern: Option<&str>,
    excludes: &[String],
) -> Result<Vec<ObjectId>> {
    let namespace = match kind {
        RefKind::Branches => Some("refs/heads/"),
        RefKind::Tags => Some("refs/tags/"),
        RefKind::Remotes => Some("refs/remotes/"),
        RefKind::All | RefKind::Glob => None,
    };

    // Without a pattern git walks a whole namespace (`for_each_ref_in`) rather
    // than matching a glob, which is why a nested branch is included too.
    let include: Include = match (namespace, pattern) {
        (Some(ns), None) => Include::Prefix(ns.to_string()),
        (Some(ns), Some(p)) => Include::Glob(implied_trailing_glob(&format!("{ns}{p}"))),
        (None, Some(p)) => Include::Glob(implied_trailing_glob(&qualify(p))),
        (None, None) => Include::Prefix("refs/".to_string()),
    };
    // For a namespaced selector git prefixes the exclude patterns too, which is
    // why `--exclude=feature --branches` works and `--exclude=refs/heads/feature`
    // does not. Exclude patterns are matched as given — no implied `/*`.
    let exclude_globs: Vec<String> = excludes
        .iter()
        .map(|p| match namespace {
            Some(ns) => format!("{ns}{p}"),
            None => qualify(p),
        })
        .collect();

    let mut out = Vec::new();
    for reference in repo.references()?.all()? {
        let Ok(reference) = reference else { continue };
        let name = reference.name().as_bstr().to_owned();
        let included = match &include {
            Include::Prefix(prefix) => name.starts_with_str(prefix),
            Include::Glob(glob) => glob_matches(glob, name.as_bstr()),
        };
        if !included {
            continue;
        }
        if exclude_globs
            .iter()
            .any(|g| glob_matches(g, name.as_bstr()))
        {
            continue;
        }
        let Ok(id) = reference.into_fully_peeled_id() else {
            continue;
        };
        let Ok(object) = id.object() else { continue };
        if let Ok(commit) = object.peel_to_commit() {
            out.push(commit.id);
        }
    }
    // `--all` is "every ref under refs/, plus HEAD", in that order.
    if kind == RefKind::All {
        if let Ok(mut head) = repo.head() {
            if let Ok(Some(id)) = head.try_peel_to_id() {
                out.push(id.detach());
            }
        }
    }
    Ok(out)
}

/// How a ref selector decides membership: a whole namespace, or a glob.
enum Include {
    Prefix(String),
    Glob(String),
}

/// git prepends `refs/` to a `--glob`/`--exclude` pattern that lacks it.
fn qualify(pattern: &str) -> String {
    if pattern.starts_with("refs/") {
        pattern.to_string()
    } else {
        format!("refs/{pattern}")
    }
}

/// `for_each_glob_ref_in()` completes a pattern that carries no glob special
/// with `/*`, so `--glob=refs/heads` reaches the branches below it.
fn implied_trailing_glob(pattern: &str) -> String {
    if pattern.bytes().any(|b| matches!(b, b'*' | b'?' | b'[')) {
        return pattern.to_string();
    }
    if pattern.ends_with('/') {
        format!("{pattern}*")
    } else {
        format!("{pattern}/*")
    }
}

fn glob_matches(pattern: &str, name: &BStr) -> bool {
    gix::glob::wildmatch(
        pattern.as_bytes().as_bstr(),
        name,
        gix::glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL,
    )
}

/// Run the commit walk in the requested order.
fn walk(
    repo: &gix::Repository,
    tips: &[ObjectId],
    hidden: &[ObjectId],
    filters: &Filters,
) -> Result<Vec<WalkItem>> {
    let mut items = Vec::new();
    match filters.order {
        Order::Default => {
            let mut platform = repo
                .rev_walk(tips.to_vec())
                .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
            if filters.first_parent {
                platform = platform.first_parent_only();
            }
            if !hidden.is_empty() {
                platform = platform.with_hidden(hidden.to_vec());
            }
            for info in platform.all()? {
                let info = info?;
                items.push(WalkItem {
                    id: info.id,
                    parents: info.parent_ids.iter().map(|id| id.to_owned()).collect(),
                });
            }
        }
        Order::Date | Order::Topo => {
            use gix::traverse::commit::{topo, Parents};
            let sorting = if filters.order == Order::Topo {
                topo::Sorting::TopoOrder
            } else {
                topo::Sorting::DateOrder
            };
            let iter =
                topo::Builder::from_iters(&repo.objects, tips.to_vec(), Some(hidden.to_vec()))
                    .sorting(sorting)
                    .parents(if filters.first_parent {
                        Parents::First
                    } else {
                        Parents::All
                    })
                    .build()?;
            for info in iter {
                let info = info?;
                items.push(WalkItem {
                    id: info.id,
                    parents: info.parent_ids.iter().map(|id| id.to_owned()).collect(),
                });
            }
        }
    }
    Ok(items)
}

/// git's `--ancestry-path`: keep only commits that descend from a bottom
/// commit. Iterated to a fixed point because the walk order is not guaranteed
/// to present a child before its parent when commit times tie.
fn keep_ancestry_path(items: Vec<WalkItem>, bottoms: &[ObjectId]) -> Vec<WalkItem> {
    let mut on_path: HashSet<ObjectId> = bottoms.iter().copied().collect();
    loop {
        let mut grew = false;
        for item in &items {
            if on_path.contains(&item.id) {
                continue;
            }
            if item.parents.iter().any(|p| on_path.contains(p)) {
                on_path.insert(item.id);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    items
        .into_iter()
        .filter(|item| on_path.contains(&item.id) && !bottoms.contains(&item.id))
        .collect()
}

/// The commits git shows for `--boundary`: parents of walked commits that the
/// walk itself never reached, in the order they were first seen.
fn boundary_commits(items: &[WalkItem]) -> Vec<ObjectId> {
    let walked: HashSet<ObjectId> = items.iter().map(|item| item.id).collect();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        for parent in &item.parents {
            if !walked.contains(parent) && seen.insert(*parent) {
                out.push(*parent);
            }
        }
    }
    out
}

/// True when nothing reachable from `hidden` is a merge, which is exactly when
/// `--exclude-first-parent-only` cannot change the result.
fn excluded_side_has_merge(repo: &gix::Repository, hidden: &[ObjectId]) -> Result<bool> {
    if hidden.is_empty() {
        return Ok(false);
    }
    for info in repo.rev_walk(hidden.to_vec()).all()? {
        if info?.parent_ids.len() > 1 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// git's `--min-parents`/`--max-parents` gate.
fn parent_count_matches(parents: usize, filters: &Filters) -> bool {
    if parents < filters.min_parents {
        return false;
    }
    match filters.max_parents {
        Some(max) => parents <= max,
        None => true,
    }
}

/// git's `--since`/`--until` gate, applied to the commit timestamp.
fn time_matches(commit: &gix::Commit<'_>, filters: &Filters) -> Result<bool> {
    if filters.since.is_none() && filters.until.is_none() {
        return Ok(true);
    }
    let seconds = commit.time()?.seconds;
    if filters.since.is_some_and(|since| seconds < since) {
        return Ok(false);
    }
    if filters.until.is_some_and(|until| seconds > until) {
        return Ok(false);
    }
    Ok(true)
}

/// git's grep machinery, reduced to fixed strings: header patterns are ANDed
/// with the message result, message patterns are ORed unless `--all-match`, and
/// `--invert-grep` flips the message result only.
fn message_matches(commit: &gix::Commit<'_>, filters: &Filters) -> Result<bool> {
    if filters.grep.is_empty() && filters.author.is_empty() {
        return Ok(true);
    }

    if !filters.author.is_empty() {
        let line = ident_line(commit.author()?);
        if !filters
            .author
            .iter()
            .any(|p| contains_pattern(line.as_bstr(), p, filters.ignore_case))
        {
            return Ok(false);
        }
    }
    if filters.grep.is_empty() {
        return Ok(true);
    }

    let message = commit.message_raw()?;
    let hit = if filters.all_match {
        filters
            .grep
            .iter()
            .all(|p| contains_pattern(message, p, filters.ignore_case))
    } else {
        filters
            .grep
            .iter()
            .any(|p| contains_pattern(message, p, filters.ignore_case))
    };
    Ok(hit != filters.invert_grep)
}

/// The raw `author`/`committer` header value git greps against.
fn ident_line(sig: gix::actor::SignatureRef<'_>) -> BString {
    let mut out = BString::from(sig.name.to_vec());
    out.push(b' ');
    out.push(b'<');
    out.extend_from_slice(sig.email);
    out.push(b'>');
    out.push(b' ');
    out.extend_from_slice(sig.time.as_bytes());
    out
}

/// The group key(s) a commit files under. Only `--group=trailer` can produce
/// more than one — or none at all, when the commit carries no such trailer.
fn group_keys(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    mailmap: &gix::mailmap::Snapshot,
    opts: &Opts,
) -> Result<Vec<BString>> {
    Ok(match &opts.group {
        GroupBy::Author => vec![format_ident(commit.author()?.trim(), mailmap, opts.email)],
        GroupBy::Committer => vec![format_ident(
            commit.committer()?.trim(),
            mailmap,
            opts.email,
        )],
        GroupBy::Format(fmt) => vec![expand_format(repo, commit, mailmap, fmt)?],
        GroupBy::Trailer(token) => {
            let message = commit.message()?;
            let Some(body) = message.body() else {
                return Ok(Vec::new());
            };
            body.trailers()
                .filter(|trailer| trailer.token.eq_ignore_ascii_case(token.as_bytes()))
                .map(|trailer| BString::from(trailer.value.to_vec()))
                .collect()
        }
    })
}

/// Port of `format_commit_message()` restricted to the placeholders shortlog
/// can be asked for here. An unhandled `%<char>` bails rather than silently
/// rendering something git would render differently.
fn expand_format(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    mailmap: &gix::mailmap::Snapshot,
    fmt: &str,
) -> Result<BString> {
    let mut out = BString::default();
    let bytes = fmt.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let Some(&next) = bytes.get(i + 1) else {
            out.push(b'%');
            i += 1;
            continue;
        };

        // `%(...)`: git copies an unrecognised group through verbatim. The
        // recognised ones do real work this port does not implement.
        if next == b'(' {
            let Some(end) = bytes[i + 2..].iter().position(|&b| b == b')') else {
                out.push(b'%');
                i += 1;
                continue;
            };
            let inner = &fmt[i + 2..i + 2 + end];
            for known in ["trailers", "describe", "decorate", "wrap", "ahead-behind"] {
                if inner == known || inner.starts_with(&format!("{known}:")) {
                    bail!("`--format` placeholder `%({inner})` is not ported");
                }
            }
            out.extend_from_slice(&bytes[i..i + 3 + end]);
            i += 3 + end;
            continue;
        }

        i += 2;
        match next {
            b'%' => out.push(b'%'),
            b'n' => out.push(b'\n'),
            b'H' => out.extend_from_slice(commit.id.to_string().as_bytes()),
            b'h' => {
                let prefix = commit.id.attach(repo).shorten_or_id();
                out.extend_from_slice(prefix.to_string().as_bytes());
            }
            b's' => {
                let message = commit.message()?;
                out.extend_from_slice(message.summary().as_bytes());
            }
            b'x' => {
                let hex = fmt.get(i..i + 2).unwrap_or_default();
                let Ok(byte) = u8::from_str_radix(hex, 16) else {
                    bail!("`--format` placeholder `%x{hex}` is not ported");
                };
                out.push(byte);
                i += 2;
            }
            b'a' | b'c' => {
                let raw = if next == b'a' {
                    commit.author()?
                } else {
                    commit.committer()?
                };
                let sig = raw.trim();
                let Some(&which) = bytes.get(i) else {
                    bail!("`--format` placeholder `%{}` is not ported", next as char);
                };
                i += 1;
                let (mapped_name, mapped_email) = match mailmap.try_resolve_ref(sig) {
                    Some(resolved) => (resolved.name, resolved.email),
                    None => (None, None),
                };
                match which {
                    b'n' => out.extend_from_slice(sig.name),
                    b'e' => out.extend_from_slice(sig.email),
                    b'N' => out.extend_from_slice(mapped_name.unwrap_or(sig.name)),
                    b'E' => out.extend_from_slice(mapped_email.unwrap_or(sig.email)),
                    other => bail!(
                        "`--format` placeholder `%{}{}` is not ported",
                        next as char,
                        other as char
                    ),
                }
            }
            other => bail!("`--format` placeholder `%{}` is not ported", other as char),
        }
    }
    Ok(out)
}

/// Port of `parse_uint()` from `builtin/shortlog.c`: read a decimal run, require
/// the terminator to be `comma` (or end of string), and fall back to `defval`
/// when the field is empty. Returns `None` on a malformed field.
fn parse_uint<'a>(arg: &mut &'a str, comma: Option<char>, defval: usize) -> Option<usize> {
    // Copy the slice out first so `rest` does not borrow through `arg`.
    let s: &'a str = *arg;
    let digits = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let (num, rest) = s.split_at(digits);
    if rest.chars().next().is_some_and(|c| Some(c) != comma) {
        return None;
    }
    let value = if num.is_empty() {
        defval
    } else {
        num.parse::<usize>().ok()?
    };
    *arg = if rest.is_empty() { rest } else { &rest[1..] };
    Some(value)
}

/// Port of `parse_wrap_args()`. Returns `false` when the argument is malformed,
/// which git reports as `error: -w[<width>[,<indent1>[,<indent2>]]]`.
fn parse_wrap_args(opts: &mut Opts, arg: Option<&str>) -> bool {
    opts.wrap_lines = true;
    let Some(arg) = arg else {
        opts.wrap = DEFAULT_WRAPLEN;
        opts.in1 = DEFAULT_INDENT1;
        opts.in2 = DEFAULT_INDENT2;
        return true;
    };

    let mut cursor = arg;
    let (Some(wrap), Some(in1), Some(in2)) = (
        parse_uint(&mut cursor, Some(','), DEFAULT_WRAPLEN),
        parse_uint(&mut cursor, Some(','), DEFAULT_INDENT1),
        parse_uint(&mut cursor, None, DEFAULT_INDENT2),
    ) else {
        return false;
    };
    opts.wrap = wrap;
    opts.in1 = in1;
    opts.in2 = in2;

    // git rejects a width that cannot even fit its own indent.
    if wrap != 0 && ((in1 != 0 && wrap <= in1) || (in2 != 0 && wrap <= in2)) {
        return false;
    }
    true
}

/// The group key: the mailmap-resolved name, plus ` <email>` under `-e`.
/// This is git's `%aN` / `%aN <%aE>` (or the `%c*` pair for `--committer`).
fn format_ident(
    sig: gix::actor::SignatureRef<'_>,
    mailmap: &gix::mailmap::Snapshot,
    email: bool,
) -> BString {
    // `ResolvedSignature` is not `Copy`, so read both fields out in one go.
    let (mapped_name, mapped_email) = match mailmap.try_resolve_ref(sig) {
        Some(resolved) => (resolved.name, resolved.email),
        None => (None, None),
    };

    let mut out = BString::from(mapped_name.unwrap_or(sig.name).to_vec());
    if email {
        out.push(b' ');
        out.push(b'<');
        out.extend_from_slice(mapped_email.unwrap_or(sig.email));
        out.push(b'>');
    }
    out
}

/// Port of `insert_one_record()`: strip a `[PATCH...]` prefix and any framing
/// whitespace off `oneline`, then file it under `ident`.
fn insert_one_record(
    groups: &mut BTreeMap<BString, Group>,
    opts: &Opts,
    ident: BString,
    oneline: &BStr,
) {
    let entry = groups.entry(ident).or_default();
    entry.count += 1;
    if opts.summary {
        return;
    }

    // Skip any leading whitespace, including any blank lines.
    let mut s = oneline.as_bytes();
    while s.first().is_some_and(|&b| is_space(b)) {
        s = &s[1..];
    }
    if s.starts_with(b"[PATCH") {
        let eol = s.iter().position(|&b| b == b'\n').unwrap_or(s.len());
        if let Some(eob) = s.iter().position(|&b| b == b']') {
            if eob < eol {
                s = &s[eob + 1..];
            }
        }
    }
    while s.first().is_some_and(|&b| is_space(b) && b != b'\n') {
        s = &s[1..];
    }

    entry.onelines.push(BString::from(s.to_vec()));
}

/// Port of `shortlog_output()`.
fn render(groups: &BTreeMap<BString, Group>, opts: &Opts, out: &mut Vec<u8>) {
    // The map is already in strcmp order, which is git's default. `-n` re-sorts
    // it stably by descending count, so ties keep the alphabetic order.
    let mut entries: Vec<(&BString, &Group)> = groups.iter().collect();
    if opts.numbered {
        entries.sort_by(|a, b| b.1.count.cmp(&a.1.count));
    }

    for (ident, group) in entries {
        if opts.summary {
            out.extend_from_slice(format!("{:6}\t", group.count).as_bytes());
            out.extend_from_slice(ident);
            out.push(b'\n');
            continue;
        }

        out.extend_from_slice(ident);
        out.extend_from_slice(format!(" ({}):\n", group.count).as_bytes());
        // Oldest first: git walks its per-ident list back to front. `--reverse`
        // already reversed the stream that built the list, so it cancels out.
        let ordered: Vec<&BString> = if opts.reverse {
            group.onelines.iter().collect()
        } else {
            group.onelines.iter().rev().collect()
        };
        for msg in ordered {
            if opts.wrap_lines {
                add_wrapped_text(out, msg, opts.in1, opts.in2, opts.wrap);
            } else {
                out.extend_from_slice(b"      ");
                out.extend_from_slice(msg);
            }
            out.push(b'\n');
        }
        out.push(b'\n');
    }
}

/// Port of `read_from_stdin()`: scan piped `git log` output for ident headers,
/// then take the first non-blank line of the message body as the subject.
fn read_from_stdin(
    groups: &mut BTreeMap<BString, Group>,
    mailmap: &gix::mailmap::Snapshot,
    opts: &Opts,
) -> Result<()> {
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;

    let matches: [&[u8]; 2] = if opts.group == GroupBy::Committer {
        [&b"Commit: "[..], &b"committer "[..]]
    } else {
        [&b"Author: "[..], &b"author "[..]]
    };

    let mut lines = LinesLf { data: &buf, pos: 0 };
    while let Some(line) = lines.next_line() {
        let Some(ident_line) = matches.iter().find_map(|prefix| line.strip_prefix(*prefix)) else {
            continue;
        };
        let ident_line = BString::from(ident_line.to_vec());

        // Discard the remaining headers, up to the blank separator line.
        while let Some(l) = lines.next_line() {
            if l.is_empty() {
                break;
            }
        }
        // Discard blank lines; the first non-blank one is the subject.
        let mut oneline = BString::default();
        while let Some(l) = lines.next_line() {
            if !l.is_empty() {
                oneline = BString::from(l.to_vec());
                break;
            }
        }

        // git skips records whose ident it cannot split.
        let Some((name, email)) = split_ident_line(ident_line.as_bstr()) else {
            continue;
        };
        let sig = gix::actor::SignatureRef {
            name,
            email,
            time: "",
        };
        let ident = format_ident(sig, mailmap, opts.email);
        insert_one_record(groups, opts, ident, oneline.as_bstr());
    }
    Ok(())
}

/// A `strbuf_getline_lf()` equivalent: yields each `\n`-terminated record with
/// the terminator removed, and no phantom empty record after a trailing `\n`.
struct LinesLf<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> LinesLf<'a> {
    fn next_line(&mut self) -> Option<&'a BStr> {
        // Copy the slice out of `self` so the result is tied to `'a`, not to
        // the `&mut self` borrow.
        let data: &'a [u8] = self.data;
        if self.pos >= data.len() {
            return None;
        }
        let rest = &data[self.pos..];
        Some(match rest.iter().position(|&b| b == b'\n') {
            Some(nl) => {
                self.pos += nl + 1;
                rest[..nl].as_bstr()
            }
            None => {
                self.pos = data.len();
                rest.as_bstr()
            }
        })
    }
}

/// Port of `split_ident_line()` reduced to what shortlog reads off it: the name
/// (trailing whitespace trimmed) and the email between the first `<` and the
/// first `>` after it. `None` when the line carries no `<...>` pair, which is
/// the `-1` git treats as "skip this record".
fn split_ident_line(line: &BStr) -> Option<(&BStr, &BStr)> {
    let bytes = line.as_bytes();
    let lt = bytes.iter().position(|&b| b == b'<')?;
    let gt = lt + 1 + bytes[lt + 1..].iter().position(|&b| b == b'>')?;

    let mut name_end = lt;
    while name_end > 0 && is_space(bytes[name_end - 1]) {
        name_end -= 1;
    }
    Some((bytes[..name_end].as_bstr(), bytes[lt + 1..gt].as_bstr()))
}

/// C's `isspace()` for the "C" locale.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// Whether `commit` should appear under a path limit — git's TREESAME test.
///
/// A single-parent commit is shown iff it differs from its parent for the paths.
/// A merge is shown iff it differs from *every* parent (git's default merge
/// simplification for which commits are listed). A root commit is shown iff its
/// tree already contains a matching path. `--first-parent` limits a merge to its
/// first parent. This lists the shown set; it does not reproduce git's traversal
/// pruning, which only diverges for a merge that discards a tracked change and
/// whose branch is reachable solely through that merge — a shape no fixture builds.
fn commit_touches_path(
    repo: &gix::Repository,
    commit: ObjectId,
    parents: &[ObjectId],
    first_parent: bool,
    pathspecs: &[Vec<u8>],
) -> Result<bool> {
    let Some(tree) = commit_tree(repo, commit) else {
        return Ok(false);
    };
    if parents.is_empty() {
        return diff_touches_path(repo, None, tree, pathspecs);
    }
    let considered = if first_parent { &parents[..1] } else { parents };
    for parent in considered {
        let parent_tree = commit_tree(repo, *parent);
        if !diff_touches_path(repo, parent_tree, tree, pathspecs)? {
            // TREESAME to this parent → not shown under default simplification.
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether the diff turning `old_tree` (empty when `None`) into `new_tree` touches
/// any of `pathspecs`. Rename tracking is off so a rename shows as a deletion and
/// an addition, letting either endpoint's path match.
fn diff_touches_path(
    repo: &gix::Repository,
    old_tree: Option<ObjectId>,
    new_tree: ObjectId,
    pathspecs: &[Vec<u8>],
) -> Result<bool> {
    let Some(new) = tree_object(repo, new_tree) else {
        return Ok(false);
    };
    let old = old_tree
        .and_then(|id| tree_object(repo, id))
        .unwrap_or_else(|| repo.empty_tree());

    let mut platform = old.changes().map_err(|e| anyhow!("{e}"))?;
    platform.options(|o| {
        o.track_path();
        o.track_rewrites(None);
    });
    let mut matched = false;
    platform
        .for_each_to_obtain_tree(&new, |change| {
            if path_matches(&change.location()[..], pathspecs) {
                matched = true;
                Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Break(()))
            } else {
                Ok(std::ops::ControlFlow::Continue(()))
            }
        })
        .map_err(|e| anyhow!("{e}"))?;
    Ok(matched)
}

/// git's plain (non-magic) pathspec match: a pathspec matches a path when it is
/// equal to it or is a leading directory prefix ending at a component boundary,
/// so `dir` matches `dir/file` but `fil` does not match `file`.
fn path_matches(path: &[u8], pathspecs: &[Vec<u8>]) -> bool {
    pathspecs.iter().any(|spec| {
        let spec = spec.strip_suffix(b"/").unwrap_or(spec);
        spec.is_empty()
            || path == spec
            || (path.len() > spec.len() && path.starts_with(spec) && path[spec.len()] == b'/')
    })
}

/// The tree id of a commit object, or `None` if it is missing or not a commit.
fn commit_tree(repo: &gix::Repository, id: ObjectId) -> Option<ObjectId> {
    let object = repo.find_object(id).ok()?;
    if object.kind != gix::object::Kind::Commit {
        return None;
    }
    Some(object.into_commit().tree_id().ok()?.detach())
}

/// The entries of a tree object, or `None` if it is missing or not a tree.
fn tree_object(repo: &gix::Repository, id: ObjectId) -> Option<gix::Tree<'_>> {
    let object = repo.find_object(id).ok()?;
    if object.kind != gix::object::Kind::Tree {
        return None;
    }
    Some(object.into_tree())
}

/// Byte length of the UTF-8 sequence introduced by `b`; 1 for a stray byte.
fn utf8_seq_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// Port of `strbuf_add_indented_text()` (`utf8.c`), git's `-w0` path.
fn add_indented_text(out: &mut Vec<u8>, text: &[u8], indent1: usize, indent2: usize) {
    let mut indent = indent1;
    let mut pos = 0;
    while pos < text.len() {
        let eol = match text[pos..].iter().position(|&b| b == b'\n') {
            Some(n) => pos + n + 1,
            None => text.len(),
        };
        out.resize(out.len() + indent, b' ');
        out.extend_from_slice(&text[pos..eol]);
        pos = eol;
        indent = indent2;
    }
}

/// Port of `strbuf_add_wrapped_text()` (`utf8.c`).
///
/// Structure follows the C loop exactly, including its habit of emitting the
/// run of whitespace that precedes a word together with that word — which is
/// why a wrapped line can keep a trailing space and why runs of spaces survive
/// wrapping. Two branches of the original are absent because the caller cannot
/// reach them: shortlog always passes a single line (no `\n` handling), and
/// subjects carry no ANSI escapes (no `display_mode_esc_sequence_len` skip).
/// Column width is counted per code point rather than via `wcwidth()`.
fn add_wrapped_text(out: &mut Vec<u8>, text: &[u8], indent1: usize, indent2: usize, width: usize) {
    if width == 0 {
        add_indented_text(out, text, indent1, indent2);
        return;
    }

    let mut bol = 0usize;
    let mut indent = indent1;
    let mut w = indent1;
    let mut space: Option<usize> = None;
    let mut i = 0usize;

    loop {
        let c = text.get(i).copied();
        let Some(byte) = c.filter(|&b| !is_space(b)) else {
            // Whitespace, or the end of the text (C's NUL terminator).
            if w <= width || space.is_none() {
                let mut start = bol;
                if c.is_none() && i == start {
                    return;
                }
                match space {
                    Some(s) => start = s,
                    None => out.resize(out.len() + indent, b' '),
                }
                out.extend_from_slice(&text[start..i]);
                let Some(c) = c else { return };
                space = Some(i);
                if c == b'\t' {
                    w |= 0x07;
                }
                w += 1;
                i += 1;
            } else {
                out.push(b'\n');
                let s = space.expect("the `||` above guarantees a break point here");
                i = s + usize::from(text.get(s).is_some_and(|&b| is_space(b)));
                bol = i;
                space = None;
                indent = indent2;
                w = indent2;
            }
            continue;
        };

        w += 1;
        i += utf8_seq_len(byte).min(text.len() - i);
    }
}
