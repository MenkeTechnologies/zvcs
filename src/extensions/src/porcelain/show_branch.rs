use anyhow::{bail, Result};
use std::collections::{HashMap, VecDeque};
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::prelude::ObjectIdExt;

/// `git show-branch` — show branches and the commits they contain, semi-visually.
///
/// This is a direct port of `builtin/show-branch.c`: the same per-rev flag bits
/// (`REV_SHIFT`), the same `join_revs` well-poisoning pass, git's own
/// `sort_in_topological_order` (an indegree walk over a LIFO or date-ordered
/// priority queue), git's `name_commits` naming heuristic, and git's
/// `CMIT_FMT_ONELINE` subject extraction. Output is therefore byte-identical to
/// stock git for the covered flags, including the `*!+- ` column marks, the
/// `[name~n]` commit names, the `---` separator row and the ANSI column colors.
///
/// Supported:
///   * `<rev>...` positionals, including glob patterns (`topic/*`), resolved
///     exactly as `append_one_rev` does
///   * `-a`/`--all`, `-r`/`--remotes`, `--current`
///   * `--more[=<n>]`, `--list`, `--independent`, `--merge-base`
///   * `--topo-order`, `--date-order`, `--sparse`, `--topics`
///   * `--no-name`, `--sha1-name`
///   * `-g`/`--reflog[=<n>[,<base>]]`, including `dwim_ref` name expansion,
///     `read_ref_at`'s reflog-by-index lookup rebuilt over `gix_ref`'s reflog
///     iterator, and a port of `show_date_relative`
///   * `--color[=<when>]`, `--no-color` (`column_colors_ansi`, with
///     `color.showbranch`/`color.ui` honoured and `auto` resolved against
///     `isatty(1)` and `TERM`)
///   * the `--no-*` negations `parse_options` synthesizes for every `OPT_BOOL`
///   * the multi-valued `showbranch.default` config, used when no argument is given
///
/// Not covered: unique-prefix abbreviations of long options.
///
/// Known deviations:
///   * commits carrying an `encoding` header are not run through
///     `logmsg_reencode` (no iconv substrate here), so their subject is emitted
///     verbatim rather than transcoded to the log output encoding
///   * a non-numeric `<base>` in `--reflog=<n>,<base>` is resolved with
///     `gix_date`'s parser rather than git's `approxidate`, which accepts a
///     wider set of fuzzy spellings
pub fn show_branch(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    // `cmd_show_branch`: with no argument at all, `showbranch.default` supplies argv.
    let mut argv: Vec<String> = args.to_vec();
    if argv.is_empty() {
        let snapshot = repo.config_snapshot();
        let values: Vec<BString> = snapshot
            .plumbing()
            .values::<BString>("showbranch.default")
            .unwrap_or_default();
        if !values.is_empty() {
            argv = values.iter().map(ToString::to_string).collect();
        }
    }

    let mut opts = Opts::new();
    let revs = match parse_args(&argv, &mut opts) {
        Ok(revs) => revs,
        Err(fail) => {
            if let Some(msg) = fail {
                eprintln!("{msg}");
            }
            eprint!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
    };

    if opts.all_heads {
        opts.all_remotes = true;
    }
    if opts.extra != 0 || opts.reflog != 0 {
        // "Listing" mode is incompatible with the independent and merge-base modes.
        if opts.independent || opts.merge_base {
            eprint!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        // `--more` in reflog mode makes no sense (`--list` is fine), and neither
        // does asking for every head at once.
        if opts.reflog != 0 && (opts.extra > 0 || opts.all_heads || opts.all_remotes) {
            eprintln!(
                "fatal: options '--reflog' and \
                 '--all/--remotes/--independent/--merge-base' cannot be used together"
            );
            return Ok(ExitCode::from(128));
        }
    }
    if opts.with_current_branch && opts.reflog != 0 {
        eprintln!("fatal: options '--reflog' and '--current' cannot be used together");
        return Ok(ExitCode::from(128));
    }

    // With no positional revs (one is allowed under --topics), default to all heads.
    if revs.len() <= usize::from(opts.topics) && !opts.all_heads && !opts.all_remotes {
        opts.all_heads = true;
    }

    let color = color_enabled(&repo, opts.color)?;

    // ---- collect the ref names to show, in git's order ----
    let mut names: Vec<String> = Vec::new();
    // Reflog mode resolves each `<ref>@{n}` to an object up front; the ordinary
    // path leaves resolution to the `repo_get_oid(ref_name[i])` loop below.
    let mut reflog_oids: Vec<ObjectId> = Vec::new();
    let mut reflog_msgs: Vec<String> = Vec::new();

    if opts.reflog != 0 {
        match collect_reflog(
            &repo,
            &revs,
            &opts,
            &mut names,
            &mut reflog_oids,
            &mut reflog_msgs,
        ) {
            Ok(()) => {}
            Err(fatal) => {
                eprintln!("fatal: {fatal}");
                return Ok(ExitCode::from(128));
            }
        }
    } else {
        for rev in &revs {
            if !append_one_rev(&repo, rev, &mut names)? {
                eprintln!("fatal: bad sha1 reference {rev}");
                return Ok(ExitCode::from(128));
            }
        }
        if opts.all_heads || opts.all_remotes {
            snarf_refs(&repo, opts.all_heads, opts.all_remotes, &mut names);
        }
    }

    // `head` is what `resolve_refdup("HEAD")` yields: the full ref name for an
    // attached HEAD, the literal "HEAD" when detached, nothing when unborn.
    let head = repo.head()?;
    let head_name: Option<String> = match &head.kind {
        gix::head::Kind::Symbolic(r) => Some(r.name.as_bstr().to_string()),
        gix::head::Kind::Detached { .. } => Some("HEAD".to_string()),
        gix::head::Kind::Unborn(_) => None,
    };
    let head_oid: Option<ObjectId> = head.id().map(|id| id.detach());
    drop(head);

    if opts.with_current_branch {
        if let Some(h) = head_name.as_deref() {
            if !names.iter().any(|n| rev_is_head(h, n)) {
                let short = h.strip_prefix("refs/heads/").unwrap_or(h).to_string();
                if !append_one_rev(&repo, &short, &mut names)? {
                    eprintln!("fatal: bad sha1 reference {short}");
                    return Ok(ExitCode::from(128));
                }
            }
        }
    }

    if names.is_empty() {
        eprintln!("No revs to be shown.");
        return Ok(ExitCode::SUCCESS);
    }

    // ---- resolve each ref name to a commit and seed its flag bit ----
    let mut g = Graph::new(&repo);
    let mut list: Vec<ObjectId> = Vec::new();
    let mut seen: Vec<ObjectId> = Vec::new();
    let mut rev_ids: Vec<ObjectId> = Vec::with_capacity(names.len());

    for (i, name) in names.iter().enumerate() {
        let resolved = match reflog_oids.get(i) {
            Some(&id) => Some(id),
            None => resolve_commit(&repo, name),
        };
        let Some(id) = resolved else {
            eprintln!("fatal: '{name}' is not a valid ref.");
            return Ok(ExitCode::from(128));
        };
        let flag = 1u32 << (REV_SHIFT + i as u32);
        g.parse(id)?;
        mark_seen(&g, id, &mut seen);
        g.or_flags(id, flag);
        if g.flags(id) == flag {
            insert_by_date(&g, &mut list, id);
        }
        rev_ids.push(id);
    }
    let num_rev = rev_ids.len();
    let rev_mask: Vec<u32> = rev_ids.iter().map(|id| g.flags(*id)).collect();

    let all_mask = (1u32 << (REV_SHIFT + num_rev as u32)) - 1;
    let all_revs = all_mask & !((1u32 << REV_SHIFT) - 1);

    if opts.extra >= 0 {
        join_revs(&mut g, &mut list, &mut seen, num_rev, opts.extra)?;
    }

    // `seen` was built by prepending; restore that order, then stable-sort by date.
    seen.reverse();
    seen.sort_by(|a, b| g.date(*b).cmp(&g.date(*a)));

    let mut out: Vec<u8> = Vec::new();

    if opts.merge_base {
        let status = show_merge_base(&mut g, &seen, all_mask, all_revs, &mut out);
        std::io::stdout().write_all(&out)?;
        return Ok(ExitCode::from(status));
    }
    if opts.independent {
        show_independent(&mut g, &rev_ids, &rev_mask, &mut out);
        std::io::stdout().write_all(&out)?;
        return Ok(ExitCode::SUCCESS);
    }

    // ---- header block: one line per named ref ----
    let mut head_at: i32 = -1;
    if num_rev > 1 || opts.extra < 0 {
        for (i, name) in names.iter().enumerate() {
            let is_head = head_name
                .as_deref()
                .is_some_and(|h| rev_is_head(h, name) && head_oid == Some(rev_ids[i]));
            if opts.extra < 0 {
                let mark = if is_head { '*' } else { ' ' };
                out.extend_from_slice(format!("{mark} [{name}] ").as_bytes());
            } else {
                out.extend(std::iter::repeat(b' ').take(i));
                let mark = if is_head { '*' } else { '!' };
                out.extend_from_slice(
                    format!(
                        "{}{mark}{} [{name}] ",
                        color_code(color, i),
                        color_reset(color)
                    )
                    .as_bytes(),
                );
            }
            match reflog_msgs.get(i) {
                // Reflog mode replaces the subject with `(<relative date>) <msg>`.
                Some(msg) => {
                    out.extend_from_slice(msg.as_bytes());
                    out.push(b'\n');
                }
                // Header lines never need a name.
                None => show_one_commit(&g, rev_ids[i], true, &mut out),
            }
            if is_head {
                head_at = i as i32;
            }
        }
        if opts.extra >= 0 {
            out.extend(std::iter::repeat(b'-').take(num_rev));
            out.push(b'\n');
        }
    }
    if opts.extra < 0 {
        std::io::stdout().write_all(&out)?;
        return Ok(ExitCode::SUCCESS);
    }

    sort_in_topological_order(&g, &mut seen, opts.date_order);

    if !opts.sha1_name && !opts.no_name {
        name_commits(&mut g, &seen, &rev_ids, &names);
    }

    // ---- the commit list, one row of marks per commit ----
    let mut extra = opts.extra;
    let mut shown_merge_point = false;
    let mut queue = VecDeque::from(seen);
    while let Some(commit) = queue.pop_front() {
        let this_flag = g.flags(commit);
        let is_merge_point = (this_flag & all_revs) == all_revs;
        shown_merge_point |= is_merge_point;

        if num_rev > 1 {
            let is_merge = g.parents(commit).len() > 1;
            if opts.topics && !is_merge_point && (this_flag & (1u32 << REV_SHIFT)) != 0 {
                continue;
            }
            if opts.dense && is_merge && omit_in_dense(&g, commit, &rev_ids) {
                continue;
            }
            for i in 0..num_rev {
                let mark = if this_flag & (1u32 << (i as u32 + REV_SHIFT)) == 0 {
                    b' '
                } else if is_merge {
                    b'-'
                } else if i as i32 == head_at {
                    b'*'
                } else {
                    b'+'
                };
                if mark == b' ' {
                    out.push(mark);
                } else {
                    out.extend_from_slice(color_code(color, i).as_bytes());
                    out.push(mark);
                    out.extend_from_slice(color_reset(color).as_bytes());
                }
            }
            out.push(b' ');
        }
        show_one_commit(&g, commit, opts.no_name, &mut out);

        if shown_merge_point {
            extra -= 1;
            if extra < 0 {
                break;
            }
        }
    }

    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// `UNINTERESTING` and `REV_SHIFT` from `builtin/show-branch.c`; `MAX_REVS` is
/// `FLAG_BITS - REV_SHIFT`, i.e. 29 - 2.
const UNINTERESTING: u32 = 1;
const REV_SHIFT: u32 = 2;
const MAX_REVS: usize = 27;
/// `DEFAULT_REFLOG` — how many entries a bare `-g`/`--reflog` asks for.
const DEFAULT_REFLOG: i32 = 4;

/// `show_branch_usage` plus the option list `usage_with_options` renders under it.
const USAGE: &str = "\
usage: git show-branch [-a | --all] [-r | --remotes] [--topo-order | --date-order]
                       [--current] [--color[=<when>] | --no-color] [--sparse]
                       [--more=<n> | --list | --independent | --merge-base]
                       [--no-name | --sha1-name] [--topics]
                       [(<rev> | <glob>)...]
   or: git show-branch (-g | --reflog)[=<n>[,<base>]] [--list] [<ref>]

    -a, --[no-]all        show remote-tracking and local branches
    -r, --[no-]remotes    show remote-tracking branches
    --[no-]color[=<when>] color '*!+-' corresponding to the branch
    --[no-]more[=<n>]     show <n> more commits after the common ancestor
    --[no-]list           synonym to more=-1
    --no-name             suppress naming strings
    --name                opposite of --no-name
    --[no-]current        include the current branch
    --[no-]sha1-name      name commits with their object names
    --[no-]merge-base     show possible merge bases
    --[no-]independent    show refs unreachable from any other ref
    --topo-order          show commits in topological order
    --[no-]topics         show only commits not on the first branch
    --[no-]sparse         show merges reachable from only one tip
    --date-order          topologically sort, maintaining date order where possible
    -g, --reflog[=<n>[,<base>]]
                          show <n> most recent ref-log entries starting at base

";

struct Opts {
    all_heads: bool,
    all_remotes: bool,
    with_current_branch: bool,
    merge_base: bool,
    independent: bool,
    no_name: bool,
    sha1_name: bool,
    topics: bool,
    /// `--sparse` clears this; git names it `dense` and defaults it to 1.
    dense: bool,
    /// `--date-order` selects `REV_SORT_BY_COMMIT_DATE`; `--topo-order` and the
    /// default are both `REV_SORT_IN_GRAPH_ORDER`.
    date_order: bool,
    /// `--more=<n>`; `--list` is `--more=-1`.
    extra: i32,
    /// How many reflog entries `-g`/`--reflog` asked for; 0 means the flag was
    /// never given, which is what git tests for.
    reflog: i32,
    /// The `<base>` half of `--reflog=<n>,<base>`: an index, or a date spec.
    reflog_base: Option<String>,
    /// `None` = unset (fall back to config), `Some(true)` = always, `Some(false)` = never.
    color: Option<bool>,
}

impl Opts {
    fn new() -> Self {
        Opts {
            all_heads: false,
            all_remotes: false,
            with_current_branch: false,
            merge_base: false,
            independent: false,
            no_name: false,
            sha1_name: false,
            topics: false,
            dense: true,
            date_order: false,
            extra: 0,
            reflog: 0,
            reflog_base: None,
            color: None,
        }
    }
}

/// A rejected command line: the `error:` line `parse_options` prints before the
/// usage block, if it printed one at all.
type ParseFail = Option<String>;

/// `OPT__COLOR`'s rejection of an unknown `<when>`.
const BAD_COLOR: &str = "error: option `color' expects \"always\", \"auto\", or \"never\"";

/// `OPTION_INTEGER`'s rejection of a non-numeric `--more` value.
const BAD_MORE: &str =
    "error: option `more' expects an integer value with an optional k/m/g suffix";

/// `parse_reflog_param()` — `<n>[,<base>]`, with an absent or zero `<n>` meaning
/// `DEFAULT_REFLOG`. The leading digit run is read `strtoul`-style, so anything
/// after it must be the `,<base>` separator.
fn parse_reflog_param(arg: &str, opts: &mut Opts) -> Result<(), ParseFail> {
    let end = arg.find(|c: char| !c.is_ascii_digit()).unwrap_or(arg.len());
    let n: i32 = arg[..end].parse().unwrap_or(0);
    let rest = &arg[end..];
    if let Some(base) = rest.strip_prefix(',') {
        opts.reflog_base = Some(base.to_string());
    } else if !rest.is_empty() {
        return Err(Some(format!("error: unrecognized reflog param '{arg}'")));
    } else {
        opts.reflog_base = None;
    }
    opts.reflog = if n == 0 { DEFAULT_REFLOG } else { n };
    Ok(())
}

/// `parse_options(..., PARSE_OPT_STOP_AT_NON_OPTION)` — option parsing stops at
/// the first non-option word; the rest is the `<rev>`/`<glob>` list.
fn parse_args(argv: &[String], opts: &mut Opts) -> Result<Vec<String>, ParseFail> {
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        if a == "--" {
            i += 1;
            break;
        }
        if !a.starts_with('-') || a == "-" {
            break;
        }
        if let Some(long) = a.strip_prefix("--") {
            match long {
                "all" => opts.all_heads = true,
                "no-all" => opts.all_heads = false,
                "remotes" => opts.all_remotes = true,
                "no-remotes" => opts.all_remotes = false,
                "current" => opts.with_current_branch = true,
                "no-current" => opts.with_current_branch = false,
                "merge-base" => opts.merge_base = true,
                "no-merge-base" => opts.merge_base = false,
                "independent" => opts.independent = true,
                "no-independent" => opts.independent = false,
                "no-name" => opts.no_name = true,
                "name" => opts.no_name = false,
                "sha1-name" => opts.sha1_name = true,
                "no-sha1-name" => opts.sha1_name = false,
                "topics" => opts.topics = true,
                "no-topics" => opts.topics = false,
                "sparse" => opts.dense = false,
                "no-sparse" => opts.dense = true,
                "topo-order" => opts.date_order = false,
                "date-order" => opts.date_order = true,
                "list" => opts.extra = -1,
                "no-list" | "no-more" => opts.extra = 0,
                // PARSE_OPT_OPTARG: bare `--more` means 1; only `--more=<n>` takes a value.
                "more" => opts.extra = 1,
                "no-color" => opts.color = Some(false),
                "color" => opts.color = Some(true),
                "reflog" => parse_reflog_param("", opts)?,
                "no-reflog" => {
                    opts.reflog = 0;
                    opts.reflog_base = None;
                }
                _ if long.starts_with("reflog=") => {
                    parse_reflog_param(&long["reflog=".len()..], opts)?;
                }
                _ if long.starts_with("more=") => {
                    let n = &long["more=".len()..];
                    opts.extra = n.parse::<i32>().map_err(|_| Some(BAD_MORE.to_string()))?;
                }
                _ if long.starts_with("color=") => {
                    opts.color = match &long["color=".len()..] {
                        "always" => Some(true),
                        "never" => Some(false),
                        "auto" => None,
                        _ => return Err(Some(BAD_COLOR.to_string())),
                    };
                }
                _ => {
                    let name = long.split('=').next().unwrap_or(long);
                    return Err(Some(format!("error: unknown option `{name}'")));
                }
            }
        } else {
            for (at, c) in a[1..].char_indices() {
                match c {
                    'a' => opts.all_heads = true,
                    'r' => opts.all_remotes = true,
                    // PARSE_OPT_OPTARG on a short option: the value, if any, is
                    // the rest of the cluster (`-g3`), never a separate word.
                    'g' => {
                        let arg = &a[1 + at + c.len_utf8()..];
                        parse_reflog_param(arg, opts)?;
                        break;
                    }
                    _ => return Err(Some(format!("error: unknown switch `{c}'"))),
                }
            }
        }
        i += 1;
    }
    Ok(argv[i..].to_vec())
}

// ---------------------------------------------------------------------------
// commit graph with git's per-object flag bits
// ---------------------------------------------------------------------------

/// The subset of `struct commit` this command needs, plus the `object.flags`
/// bitfield show-branch drives its whole algorithm from. `flags` is keyed by id
/// independently of the parsed data, so an unparsed parent can still be poisoned
/// with `UNINTERESTING` exactly as the C postprocess pass does.
struct Graph<'r> {
    repo: &'r gix::Repository,
    flags: HashMap<ObjectId, u32>,
    parents: HashMap<ObjectId, Vec<ObjectId>>,
    dates: HashMap<ObjectId, i64>,
    /// Filled in by [`name_commits`]; empty under `--no-name`/`--sha1-name`.
    names: HashMap<ObjectId, CommitName>,
}

impl<'r> Graph<'r> {
    fn new(repo: &'r gix::Repository) -> Self {
        Graph {
            repo,
            flags: HashMap::new(),
            parents: HashMap::new(),
            dates: HashMap::new(),
            names: HashMap::new(),
        }
    }

    /// `parse_commit()` — decode parents and the committer date once per object.
    fn parse(&mut self, id: ObjectId) -> Result<()> {
        if self.parents.contains_key(&id) {
            return Ok(());
        }
        let commit = self.repo.find_object(id)?.into_commit();
        let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
        let date = commit.time()?.seconds;
        self.parents.insert(id, parents);
        self.dates.insert(id, date);
        Ok(())
    }

    fn flags(&self, id: ObjectId) -> u32 {
        self.flags.get(&id).copied().unwrap_or(0)
    }

    fn or_flags(&mut self, id: ObjectId, bits: u32) {
        *self.flags.entry(id).or_insert(0) |= bits;
    }

    /// Parents of a parsed commit; empty for one never parsed, as in C.
    fn parents(&self, id: ObjectId) -> &[ObjectId] {
        self.parents.get(&id).map_or(&[][..], Vec::as_slice)
    }

    fn date(&self, id: ObjectId) -> i64 {
        self.dates.get(&id).copied().unwrap_or(0)
    }

    /// The raw commit message: everything past the header block.
    fn message(&self, id: ObjectId) -> Vec<u8> {
        match self.repo.find_object(id) {
            Ok(object) => object.into_commit().message_raw_sloppy().to_vec(),
            Err(_) => Vec::new(),
        }
    }
}

/// `mark_seen()` — a commit enters `seen` exactly once, before any flag is set.
fn mark_seen(g: &Graph<'_>, id: ObjectId, seen: &mut Vec<ObjectId>) -> bool {
    if g.flags(id) == 0 {
        seen.push(id);
        true
    } else {
        false
    }
}

/// `commit_list_insert_by_date()` — newest first, inserting before the first
/// entry that is strictly older so equal dates keep insertion order.
fn insert_by_date(g: &Graph<'_>, list: &mut Vec<ObjectId>, id: ObjectId) {
    let date = g.date(id);
    let pos = list
        .iter()
        .position(|c| g.date(*c) < date)
        .unwrap_or(list.len());
    list.insert(pos, id);
}

/// `join_revs()` — walk down from the tips propagating each rev's bit until the
/// frontier stops being interesting, then poison every ancestor of a merge base.
fn join_revs(
    g: &mut Graph<'_>,
    list: &mut Vec<ObjectId>,
    seen: &mut Vec<ObjectId>,
    num_rev: usize,
    mut extra: i32,
) -> Result<()> {
    let all_mask = (1u32 << (REV_SHIFT + num_rev as u32)) - 1;
    let all_revs = all_mask & !((1u32 << REV_SHIFT) - 1);

    while !list.is_empty() {
        let still_interesting = list.iter().any(|c| g.flags(*c) & UNINTERESTING == 0);
        let commit = list.remove(0);
        let mut flags = g.flags(commit) & all_mask;

        if !still_interesting && extra <= 0 {
            break;
        }

        mark_seen(g, commit, seen);
        if (flags & all_revs) == all_revs {
            flags |= UNINTERESTING;
        }

        for p in g.parents(commit).to_vec() {
            let this_flag = g.flags(p);
            if (this_flag & flags) == flags {
                continue;
            }
            g.parse(p)?;
            if mark_seen(g, p, seen) && !still_interesting {
                extra -= 1;
            }
            g.or_flags(p, flags);
            insert_by_date(g, list, p);
        }
    }

    // Complete the well-poisoning: anything reachable from a merge base, or from
    // an already-uninteresting commit, is uninteresting too.
    loop {
        let mut changed = false;
        for c in seen.clone() {
            let f = g.flags(c);
            if (f & all_revs) != all_revs && (f & UNINTERESTING) == 0 {
                continue;
            }
            for p in g.parents(c).to_vec() {
                if g.flags(p) & UNINTERESTING == 0 {
                    g.or_flags(p, UNINTERESTING);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    Ok(())
}

/// `--merge-base`: print every commit reachable from all revs that is not itself
/// an ancestor of another such commit. Exit status 1 when none was found.
fn show_merge_base(
    g: &mut Graph<'_>,
    seen: &[ObjectId],
    all_mask: u32,
    all_revs: u32,
    out: &mut Vec<u8>,
) -> u8 {
    let mut status = 1;
    for &commit in seen {
        let flags = g.flags(commit) & all_mask;
        if (flags & UNINTERESTING) == 0 && (flags & all_revs) == all_revs {
            out.extend_from_slice(format!("{commit}\n").as_bytes());
            status = 0;
            g.or_flags(commit, UNINTERESTING);
        }
    }
    status
}

/// `--independent`: print the tips that no other tip can reach.
fn show_independent(g: &mut Graph<'_>, rev: &[ObjectId], rev_mask: &[u32], out: &mut Vec<u8>) {
    for (i, &commit) in rev.iter().enumerate() {
        if g.flags(commit) == rev_mask[i] {
            out.extend_from_slice(format!("{commit}\n").as_bytes());
        }
        g.or_flags(commit, UNINTERESTING);
    }
}

/// `omit_in_dense()` — hide merges only one shown tip can reach, unless the merge
/// is itself one of the tips.
fn omit_in_dense(g: &Graph<'_>, commit: ObjectId, rev: &[ObjectId]) -> bool {
    if rev.contains(&commit) {
        return false;
    }
    let flag = g.flags(commit);
    let count = (0..rev.len())
        .filter(|&i| flag & (1u32 << (i as u32 + REV_SHIFT)) != 0)
        .count();
    count == 1
}

// ---------------------------------------------------------------------------
// git's topological sort (commit.c) over its prio-queue (prio-queue.c)
// ---------------------------------------------------------------------------

/// A port of git's `struct prio_queue`: a plain LIFO stack when no comparison is
/// configured, otherwise an array-backed binary heap whose ties break on
/// insertion order.
struct PrioQueue {
    array: Vec<(usize, ObjectId)>,
    ctr: usize,
    by_date: bool,
}

impl PrioQueue {
    fn new(by_date: bool) -> Self {
        PrioQueue {
            array: Vec::new(),
            ctr: 0,
            by_date,
        }
    }

    /// `compare()`: newer commits first, then insertion order.
    fn compare(&self, g: &Graph<'_>, i: usize, j: usize) -> i64 {
        let (a, b) = (g.date(self.array[i].1), g.date(self.array[j].1));
        let cmp = match a.cmp(&b) {
            std::cmp::Ordering::Less => 1,
            std::cmp::Ordering::Greater => -1,
            std::cmp::Ordering::Equal => 0,
        };
        if cmp != 0 {
            cmp
        } else {
            self.array[i].0 as i64 - self.array[j].0 as i64
        }
    }

    fn put(&mut self, g: &Graph<'_>, id: ObjectId) {
        self.array.push((self.ctr, id));
        self.ctr += 1;
        if !self.by_date {
            return;
        }
        let mut ix = self.array.len() - 1;
        while ix > 0 {
            let parent = (ix - 1) / 2;
            if self.compare(g, parent, ix) <= 0 {
                break;
            }
            self.array.swap(parent, ix);
            ix = parent;
        }
    }

    fn get(&mut self, g: &Graph<'_>) -> Option<ObjectId> {
        if self.array.is_empty() {
            return None;
        }
        if !self.by_date {
            return self.array.pop().map(|e| e.1);
        }
        let result = self.array[0].1;
        let last = self.array.pop().expect("checked non-empty above");
        if self.array.is_empty() {
            return Some(result);
        }
        self.array[0] = last;

        let len = self.array.len();
        let mut ix = 0;
        while ix * 2 + 1 < len {
            let mut child = ix * 2 + 1;
            if child + 1 < len && self.compare(g, child, child + 1) >= 0 {
                child += 1;
            }
            if self.compare(g, ix, child) <= 0 {
                break;
            }
            self.array.swap(child, ix);
            ix = child;
        }
        Some(result)
    }

    /// `prio_queue_reverse()` — LIFO only, so the initial tips come out in the
    /// order the caller supplied them.
    fn reverse(&mut self) {
        self.array.reverse();
    }
}

/// `sort_in_topological_order()` — a parent is emitted only once every child of
/// it that is present in the list has been emitted.
fn sort_in_topological_order(g: &Graph<'_>, list: &mut Vec<ObjectId>, by_date: bool) {
    if list.is_empty() {
        return;
    }
    let orig = std::mem::take(list);

    let mut indegree: HashMap<ObjectId, i32> = HashMap::new();
    for c in &orig {
        indegree.insert(*c, 1);
    }
    for c in &orig {
        for p in g.parents(*c) {
            if let Some(pi) = indegree.get_mut(p) {
                if *pi != 0 {
                    *pi += 1;
                }
            }
        }
    }

    let mut queue = PrioQueue::new(by_date);
    for c in &orig {
        if indegree.get(c).copied().unwrap_or(0) == 1 {
            queue.put(g, *c);
        }
    }
    if !by_date {
        queue.reverse();
    }

    while let Some(commit) = queue.get(g) {
        for p in g.parents(commit).to_vec() {
            if let Some(pi) = indegree.get_mut(&p) {
                if *pi != 0 {
                    *pi -= 1;
                    if *pi == 1 {
                        queue.put(g, p);
                    }
                }
            }
        }
        indegree.insert(commit, 0);
        list.push(commit);
    }
}

// ---------------------------------------------------------------------------
// commit naming (`name_commits`)
// ---------------------------------------------------------------------------

/// `struct commit_name`: which head this commit descends from, and how many
/// first-parent hops away it is.
#[derive(Clone)]
struct CommitName {
    head_name: String,
    generation: i32,
}

/// `name_first_parent_chain()` — extend a name down the first-parent chain for as
/// long as the next parent is still unnamed; returns how many it named.
fn name_first_parent_chain(
    g: &Graph<'_>,
    names: &mut HashMap<ObjectId, CommitName>,
    mut c: ObjectId,
) -> usize {
    let mut i = 0;
    loop {
        let Some(cn) = names.get(&c).cloned() else { break };
        let Some(&p) = g.parents(c).first() else { break };
        if names.contains_key(&p) {
            break;
        }
        names.insert(
            p,
            CommitName {
                head_name: cn.head_name,
                generation: cn.generation + 1,
            },
        );
        i += 1;
        c = p;
    }
    i
}

/// `name_commits()` — name the tips, then their first-parent ancestries, then
/// reach the remainder through `^<n>` side-parent suffixes.
fn name_commits(g: &mut Graph<'_>, list: &[ObjectId], rev: &[ObjectId], ref_name: &[String]) {
    let mut names: HashMap<ObjectId, CommitName> = HashMap::new();

    for &c in list {
        if names.contains_key(&c) {
            continue;
        }
        if let Some(i) = rev.iter().position(|r| *r == c) {
            names.insert(
                c,
                CommitName {
                    head_name: ref_name[i].clone(),
                    generation: 0,
                },
            );
        }
    }

    loop {
        let mut i = 0;
        for &c in list {
            i += name_first_parent_chain(g, &mut names, c);
        }
        if i == 0 {
            break;
        }
    }

    loop {
        let mut i = 0;
        for &c in list {
            let Some(n) = names.get(&c).cloned() else {
                continue;
            };
            let mut nth = 0;
            for p in g.parents(c).to_vec() {
                nth += 1;
                if names.contains_key(&p) {
                    continue;
                }
                let mut newname = match n.generation {
                    0 => n.head_name.clone(),
                    1 => format!("{}^", n.head_name),
                    gen => format!("{}~{gen}", n.head_name),
                };
                if nth == 1 {
                    newname.push('^');
                } else {
                    newname.push_str(&format!("^{nth}"));
                }
                names.insert(
                    p,
                    CommitName {
                        head_name: newname,
                        generation: 0,
                    },
                );
                i += 1;
                name_first_parent_chain(g, &mut names, p);
            }
        }
        if i == 0 {
            break;
        }
    }

    g.names = names;
}

// ---------------------------------------------------------------------------
// output
// ---------------------------------------------------------------------------

/// `show_one_commit()` — an optional `[name]` prefix followed by the commit's
/// `CMIT_FMT_ONELINE` subject.
fn show_one_commit(g: &Graph<'_>, id: ObjectId, no_name: bool, out: &mut Vec<u8>) {
    if !no_name {
        match g.names.get(&id) {
            Some(name) => {
                out.extend_from_slice(format!("[{}", name.head_name).as_bytes());
                if name.generation == 1 {
                    out.push(b'^');
                } else if name.generation != 0 {
                    out.extend_from_slice(format!("~{}", name.generation).as_bytes());
                }
                out.extend_from_slice(b"] ");
            }
            None => {
                let short = id.attach(g.repo).shorten_or_id().to_string();
                out.extend_from_slice(format!("[{short}] ").as_bytes());
            }
        }
    }
    let mut subject = oneline_subject(&g.message(id));
    const PATCH: &[u8] = b"[PATCH] ";
    if subject.starts_with(PATCH) {
        subject.drain(..PATCH.len());
    }
    out.extend_from_slice(&subject);
    out.push(b'\n');
}

/// `get_one_line()` — the byte length of the next line, newline included.
fn one_line_len(msg: &[u8]) -> usize {
    match msg.iter().position(|&c| c == b'\n') {
        Some(i) => i + 1,
        None => msg.len(),
    }
}

/// `is_blank_line()`'s side effect — the line with its newline and any trailing
/// whitespace removed.
fn rtrim_line(line: &[u8]) -> &[u8] {
    let mut len = line.len();
    while len > 0 && line[len - 1] == b'\n' {
        len -= 1;
    }
    while len > 0 && line[len - 1].is_ascii_whitespace() {
        len -= 1;
    }
    &line[..len]
}

/// `skip_blank_lines()`, then `format_subject(sb, msg, " ")`, then
/// `strbuf_rtrim()` — what `pp_commit_easy(CMIT_FMT_ONELINE, ...)` leaves behind.
fn oneline_subject(msg: &[u8]) -> Vec<u8> {
    let mut rest = msg;
    loop {
        let len = one_line_len(rest);
        if len == 0 || !rtrim_line(&rest[..len]).is_empty() {
            break;
        }
        rest = &rest[len..];
    }

    let mut out: Vec<u8> = Vec::new();
    let mut first = true;
    loop {
        let len = one_line_len(rest);
        if len == 0 {
            break;
        }
        let line = rtrim_line(&rest[..len]);
        rest = &rest[len..];
        if line.is_empty() {
            break;
        }
        if !first {
            out.push(b' ');
        }
        out.extend_from_slice(line);
        first = false;
    }
    while out.last().is_some_and(u8::is_ascii_whitespace) {
        out.pop();
    }
    out
}

/// `column_colors_ansi` — the 12 column colors show-branch cycles through
/// (`column_colors_ansi_max` excludes the trailing reset entry).
const COLUMN_COLORS: [&str; 12] = [
    "\x1b[31m",
    "\x1b[32m",
    "\x1b[33m",
    "\x1b[34m",
    "\x1b[35m",
    "\x1b[36m",
    "\x1b[1;31m",
    "\x1b[1;32m",
    "\x1b[1;33m",
    "\x1b[1;34m",
    "\x1b[1;35m",
    "\x1b[1;36m",
];

fn color_code(on: bool, idx: usize) -> &'static str {
    if on {
        COLUMN_COLORS[idx % COLUMN_COLORS.len()]
    } else {
        ""
    }
}

fn color_reset(on: bool) -> &'static str {
    if on {
        "\x1b[m"
    } else {
        ""
    }
}

/// `want_color()` — the command line wins, then `color.showbranch`, then
/// `color.ui`; `auto` (the default) means an interactive stdout on a real term.
fn color_enabled(repo: &gix::Repository, cli: Option<bool>) -> Result<bool> {
    if let Some(v) = cli {
        return Ok(v);
    }
    let snapshot = repo.config_snapshot();
    for key in ["color.showbranch", "color.ui"] {
        if let Some(raw) = snapshot.string(key) {
            let value = raw.to_str_lossy().to_lowercase();
            return Ok(match value.as_str() {
                "always" => true,
                "auto" => auto_color(),
                "true" | "yes" | "on" | "1" => true,
                "false" | "no" | "off" | "0" | "" => false,
                other => bail!("invalid value {other:?} for {key}"),
            });
        }
        // A valueless key (`[color]\n\tui`) is boolean true.
        if let Some(b) = snapshot.boolean(key) {
            return Ok(b);
        }
    }
    Ok(auto_color())
}

/// `check_auto_color()` — a tty on stdout, and a `TERM` that is not `dumb`.
fn auto_color() -> bool {
    if !std::io::stdout().is_terminal() {
        return false;
    }
    matches!(std::env::var("TERM"), Ok(t) if t != "dumb")
}

// ---------------------------------------------------------------------------
// ref collection (`append_*_ref`, `snarf_refs`, `append_one_rev`)
// ---------------------------------------------------------------------------

/// `append_ref()` — record a name, skipping duplicates and anything that does not
/// peel to a commit, and warn once the 26-rev ceiling is reached.
fn append_ref(repo: &gix::Repository, refname: &str, names: &mut Vec<String>) {
    if resolve_commit(repo, refname).is_none() {
        return;
    }
    if names.iter().any(|n| n == refname) {
        return;
    }
    if names.len() >= MAX_REVS {
        eprintln!("warning: ignoring {refname}; cannot handle more than {MAX_REVS} refs");
        return;
    }
    names.push(refname.to_string());
}

/// `append_head_ref()`/`append_remote_ref()` — shorten `refs/heads/x` to `x` (or
/// `refs/remotes/a/b` to `a/b`), falling back to the `heads/`-qualified form when
/// the short name would resolve elsewhere (e.g. a tag of the same name).
fn append_short_ref(
    repo: &gix::Repository,
    refname: &str,
    full_prefix: &str,
    target: ObjectId,
    names: &mut Vec<String>,
) {
    let Some(short) = refname.strip_prefix(full_prefix) else {
        return;
    };
    let unambiguous = repo
        .rev_parse_single(short)
        .is_ok_and(|id| id.detach() == target);
    let name = if unambiguous {
        short.to_string()
    } else {
        refname["refs/".len()..].to_string()
    };
    append_ref(repo, &name, names);
}

/// `snarf_refs()` — append local heads and/or remote-tracking branches, each
/// appended range sorted with git's `version_cmp`.
fn snarf_refs(repo: &gix::Repository, heads: bool, remotes: bool, names: &mut Vec<String>) {
    for (want, prefix) in [(heads, "refs/heads/"), (remotes, "refs/remotes/")] {
        if !want {
            continue;
        }
        let start = names.len();
        if let Ok(platform) = repo.references() {
            if let Ok(iter) = platform.all() {
                for reference in iter {
                    let Ok(mut reference) = reference else { continue };
                    let name = reference.name().as_bstr().to_string();
                    if !name.starts_with(prefix) {
                        continue;
                    }
                    let Ok(id) = reference.follow_to_object() else {
                        continue;
                    };
                    append_short_ref(repo, &name, prefix, id.detach(), names);
                }
            }
        }
        names[start..].sort_by(|a, b| version_cmp(a.as_bytes(), b.as_bytes()).cmp(&0));
    }
}

/// `append_one_rev()` — a literal revision if it resolves, else a glob matched
/// against every ref. `false` means git's `die("bad sha1 reference %s")`.
fn append_one_rev(repo: &gix::Repository, av: &str, names: &mut Vec<String>) -> Result<bool> {
    if repo.rev_parse_single(av).is_ok() {
        append_ref(repo, av, names);
        return Ok(true);
    }
    if av.contains(['*', '?', '[']) {
        let start = names.len();
        append_matching_refs(repo, av, names);
        if names.len() == start && names.len() < MAX_REVS {
            eprintln!("error: no matching refs with {av}");
        }
        names[start..].sort_by(|a, b| version_cmp(a.as_bytes(), b.as_bytes()).cmp(&0));
        return Ok(true);
    }
    Ok(false)
}

/// `append_matching_ref()` — the pattern is matched against the tail of the ref
/// name carrying the same number of slashes as the pattern itself.
fn append_matching_refs(repo: &gix::Repository, pattern: &str, names: &mut Vec<String>) {
    let pattern_slashes = pattern.matches('/').count();
    let Ok(platform) = repo.references() else {
        return;
    };
    let Ok(iter) = platform.all() else { return };

    for reference in iter {
        let Ok(mut reference) = reference else { continue };
        let refname = reference.name().as_bstr().to_string();

        // Drop leading path components until the tail has as many slashes as the
        // pattern; the walk only ever stops just past an ASCII '/' or at the end.
        let bytes = refname.as_bytes();
        let mut slash = bytes.iter().filter(|&&c| c == b'/').count();
        let mut pos = 0;
        while pos < bytes.len() && pattern_slashes < slash {
            if bytes[pos] == b'/' {
                slash -= 1;
            }
            pos += 1;
        }
        if pos >= bytes.len() {
            continue;
        }
        let tail = &bytes[pos..];
        if !gix::glob::wildmatch(
            pattern.as_bytes().as_bstr(),
            tail.as_bstr(),
            gix::glob::wildmatch::Mode::empty(),
        ) {
            continue;
        }

        let Ok(id) = reference.follow_to_object() else {
            continue;
        };
        let id = id.detach();
        if refname.starts_with("refs/heads/") {
            append_short_ref(repo, &refname, "refs/heads/", id, names);
        } else if refname.starts_with("refs/tags/") {
            append_ref(repo, &refname["refs/".len()..], names);
        } else {
            append_ref(repo, &refname, names);
        }
    }
}

/// `rev_is_head()` — does the resolved `HEAD` ref name denote this shown ref?
fn rev_is_head(head: &str, name: &str) -> bool {
    let head = head.strip_prefix("refs/heads/").unwrap_or(head);
    let name = match name.strip_prefix("refs/heads/") {
        Some(n) => n,
        None => name.strip_prefix("heads/").unwrap_or(name),
    };
    head == name
}

/// `lookup_commit_reference()` applied to `get_oid()` — resolve a name, peel to a
/// commit, and report failure as `None`.
fn resolve_commit(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let id = repo.rev_parse_single(spec).ok()?;
    let object = id.object().ok()?;
    object.peel_to_commit().ok().map(|c| c.id)
}

// ---------------------------------------------------------------------------
// reflog mode (`-g`/`--reflog`): dwim_ref + read_ref_at + show_date_relative
// ---------------------------------------------------------------------------

/// `ref_rev_parse_rules` from `refs.c`, with `{}` standing in for `%.*s`.
const REF_REV_PARSE_RULES: [&str; 6] = [
    "{}",
    "refs/{}",
    "refs/tags/{}",
    "refs/heads/{}",
    "refs/remotes/{}",
    "refs/remotes/{}/HEAD",
];

/// `repo_dwim_ref()` — expand a short name through `ref_rev_parse_rules` and
/// return the *resolved* full ref name (symrefs followed) with its object id.
fn dwim_ref(repo: &gix::Repository, name: &str) -> Option<(String, ObjectId)> {
    'rules: for rule in REF_REV_PARSE_RULES {
        let full = rule.replace("{}", name);
        let Ok(mut reference) = repo.find_reference(full.as_str()) else {
            continue;
        };
        // `refs_resolve_ref_unsafe` reports the name it landed on, not the one
        // it was handed, so a symbolic ref names its ultimate target. A dangling
        // symref resolves to nothing and the next rule gets its turn.
        loop {
            let next = match reference.follow() {
                Some(Ok(next)) => next,
                Some(Err(_)) => continue 'rules,
                None => break,
            };
            reference = next;
        }
        let resolved = reference.name().as_bstr().to_string();
        let gix::refs::TargetRef::Object(id) = reference.target() else {
            continue;
        };
        return Some((resolved, id.to_owned()));
    }
    None
}

/// Every reflog entry of `full`, newest first — the order
/// `refs_for_each_reflog_ent_reverse` feeds `read_ref_at_ent`.
fn reflog_entries(repo: &gix::Repository, full: &str) -> Vec<gix::refs::log::Line> {
    let Ok(reference) = repo.find_reference(full) else {
        return Vec::new();
    };
    let mut platform = reference.log_iter();
    let mut lines: Vec<gix::refs::log::Line> = Vec::new();
    if let Ok(Some(iter)) = platform.all() {
        for line in iter {
            let Ok(line) = line else { break };
            lines.push(line.to_owned());
        }
    }
    lines.reverse();
    lines
}

/// The `<base>` of `--reflog=<n>,<base>`: an index outright, or — as git does
/// with `approxidate` plus `read_ref_at(at_time, -1)` — the position of the
/// newest entry no younger than the given date.
fn reflog_base_index(base: Option<&str>, entries: &[gix::refs::log::Line]) -> i32 {
    let Some(base) = base else { return 0 };
    let end = base
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(base.len());
    if end == base.len() {
        return base.parse().unwrap_or(0);
    }
    let Ok(at) = gix::date::parse(base, Some(std::time::SystemTime::now())) else {
        return 0;
    };
    entries
        .iter()
        .position(|e| e.signature.time.seconds <= at.seconds)
        .map_or(0, |i| i as i32)
}

/// The reflog half of `cmd_show_branch`: turn `<ref>` into a run of
/// `<ref>@{n}` pseudo-refs with their object ids and `(<date>) <msg>` captions.
/// `Err` carries the text of git's `die()`.
fn collect_reflog(
    repo: &gix::Repository,
    revs: &[String],
    opts: &Opts,
    names: &mut Vec<String>,
    oids: &mut Vec<ObjectId>,
    msgs: &mut Vec<String>,
) -> Result<(), String> {
    // With no argument at all git substitutes the ref HEAD resolves to.
    let mut av: Vec<String> = revs.to_vec();
    if av.is_empty() {
        let head = repo.head().map_err(|e| e.to_string())?;
        let fake = match &head.kind {
            gix::head::Kind::Symbolic(r) => r.name.as_bstr().to_string(),
            gix::head::Kind::Detached { .. } => "HEAD".to_string(),
            gix::head::Kind::Unborn(_) => {
                return Err("no branches given, and HEAD is not valid".into())
            }
        };
        av.push(fake);
    }
    if av.len() != 1 {
        return Err("--reflog option needs one branch name".into());
    }
    if opts.reflog > MAX_REVS as i32 {
        return Err(format!("only {MAX_REVS} entries can be shown at one time."));
    }
    let Some((full, dwim_oid)) = dwim_ref(repo, &av[0]) else {
        return Err(format!("no such ref {}", av[0]));
    };

    let entries = reflog_entries(repo, &full);
    let base = reflog_base_index(opts.reflog_base.as_deref(), &entries);

    for i in 0..opts.reflog {
        let nth = base + i;
        // `read_ref_at` only tolerates an empty log for `@{0}`, where the ref's
        // own value is the documented fallback; past that it is fatal.
        if entries.is_empty() {
            if nth == 0 {
                break;
            }
            return Err(format!("log for {full} is empty"));
        }
        let Some(entry) = usize::try_from(nth).ok().and_then(|n| entries.get(n)) else {
            break;
        };
        // At `@{0}` git keeps the id `dwim_ref` produced rather than the one the
        // log records; the two only diverge on a corrupt log.
        let oid = if nth == 0 { dwim_oid } else { entry.new_oid };
        // `append_ref()` drops anything that does not peel to a commit.
        if resolve_commit(repo, &oid.to_string()).is_none() {
            continue;
        }
        if names.len() >= MAX_REVS {
            eprintln!(
                "warning: ignoring {}@{{{nth}}}; cannot handle more than {MAX_REVS} refs",
                av[0]
            );
            break;
        }
        // git truncates the log message at the first newline and substitutes
        // "(none)" for an empty one.
        let raw = entry.message.to_str_lossy();
        let first = raw.split('\n').next().unwrap_or("");
        let msg = if first.is_empty() { "(none)" } else { first };
        msgs.push(format!(
            "({}) {msg}",
            show_date_relative(entry.signature.time.seconds)
        ));
        names.push(format!("{}@{{{nth}}}", av[0]));
        oids.push(oid);
    }
    Ok(())
}

/// `show_date_relative()` from `date.c`, verbatim thresholds and roundings.
fn show_date_relative(secs: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    if now < secs {
        return "in the future".to_string();
    }
    let mut diff = now - secs;
    if diff < 90 {
        return ago(diff, "second");
    }
    diff = (diff + 30) / 60;
    if diff < 90 {
        return ago(diff, "minute");
    }
    diff = (diff + 30) / 60;
    if diff < 36 {
        return ago(diff, "hour");
    }
    // Everything below is counted in days.
    diff = (diff + 12) / 24;
    if diff < 14 {
        return ago(diff, "day");
    }
    if diff < 70 {
        return ago((diff + 3) / 7, "week");
    }
    if diff < 365 {
        return ago((diff + 15) / 30, "month");
    }
    if diff < 1825 {
        let total_months = (diff * 12 * 2 + 365) / (365 * 2);
        let years = total_months / 12;
        let months = total_months % 12;
        if months != 0 {
            let plural = if years == 1 { "" } else { "s" };
            return format!("{years} year{plural}, {}", ago(months, "month"));
        }
        return ago(years, "year");
    }
    ago((diff + 183) / 365, "year")
}

/// The `Q_("%d <unit> ago", "%d <unit>s ago", n)` half of `show_date_relative`.
fn ago(n: i64, unit: &str) -> String {
    let plural = if n == 1 { "" } else { "s" };
    format!("{n} {unit}{plural} ago")
}

/// `find_digit_prefix()` — consume a run of digits at `i`, returning its value.
fn find_digit_prefix(s: &[u8], i: &mut usize) -> i64 {
    let mut ver: i64 = 0;
    while let Some(&c) = s.get(*i) {
        if !c.is_ascii_digit() {
            break;
        }
        ver = ver * 10 + i64::from(c - b'0');
        *i += 1;
    }
    ver
}

/// `version_cmp()` — digit runs compare numerically, every other byte compares as
/// C's (signed) `char` does on the supported targets.
fn version_cmp(a: &[u8], b: &[u8]) -> i64 {
    let (mut ai, mut bi) = (0usize, 0usize);
    loop {
        let va = find_digit_prefix(a, &mut ai);
        let vb = find_digit_prefix(b, &mut bi);
        if va != vb {
            return va - vb;
        }
        loop {
            let mut ca = i64::from(a.get(ai).map_or(0, |&c| c as i8));
            let mut cb = i64::from(b.get(bi).map_or(0, |&c| c as i8));
            if (0x30..=0x39).contains(&ca) {
                ca = 0;
            }
            if (0x30..=0x39).contains(&cb) {
                cb = 0;
            }
            if ca != cb {
                return ca - cb;
            }
            if ca == 0 {
                break;
            }
            ai += 1;
            bi += 1;
        }
        if ai >= a.len() && bi >= b.len() {
            return 0;
        }
    }
}
