//! `git last-modified` — show which commit last modified each path.
//!
//! This is a faithful port of `builtin/last-modified.c` (git 2.56), reproducing stock
//! output byte-for-byte, including the *emission order*, which is not sorted: git snapshots its
//! path hashmap into an array at startup and emits in that array's order,
//! grouped by the commit that resolved them. Both the hashmap iteration order
//! (`hashmap.c` + `strhash`/FNV-1a) and the commit priority-queue order
//! (`compare_commits_by_gen_then_commit_date`, FIFO on ties) are reproduced.
//!
//! Covered:
//!   * `-r`/`--recursive` (and `--no-recursive`), `-t`/`--show-trees`
//!     (and `--no-show-trees`), `--max-depth=<n>` / `--max-depth <n>`, `-z`
//!   * an optional `<revision-range>` (defaults to `HEAD`) of `A..B`, `A...B` (the merge
//!     bases excluded), `^X`, `X^@`, `X^!` and `X^-[<n>]` operands, with `--not` and
//!     `--all` among them: the excluded side seeds the `--not` queue and paths that
//!     reach it are emitted against the `^`-prefixed boundary commit
//!   * `-n`/`--max-count` and the rest of `handle_revision_opt()`'s count-and-age arm:
//!     each commit popped past the limit is a boundary as well
//!   * literal `[--] <pathspec>...`, prefixed with the repo-relative cwd like
//!     git does, with git's exact depth rule (`tree-diff.c:check_recursion_depth`)
//!   * C-style path quoting (`core.quotePath`) for the newline-terminated form
//!   * the argv diagnostics: `-h` (usage on stdout, exit 0), an unrecognised
//!     option (`error: unknown last-modified argument:` + usage on stderr, 129),
//!     `setup_revisions`' three `verify_filename`/`verify_non_filename` fatals,
//!     and `last-modified can only operate on one commit at a time`
//!
//! Not covered — these `bail!` rather than emit output that would diverge:
//!   * pathspec magic (`:(...)`)

use anyhow::Result;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;

/// `last_modified_usage[]` rendered by `usage_with_options()`; the option block
/// is what `parse_options` derives from `last_modified_options[]`.
const USAGE: &str = "\
usage: git last-modified [--recursive] [--show-trees] [--max-depth=<depth>] [-z]
                         [<revision-range>] [[--] <pathspec>...]

    -r, --[no-]recursive  recurse into subtrees
    -t, --[no-]show-trees show tree entries when recursing into subtrees
    --max-depth <n>       maximum tree depth to recurse
    -z                    lines are separated with NUL character

";

/// `verify_filename(..., diagnose_misspelt_rev = 1)`: the argument was neither a
/// revision nor an existing path, so git cannot tell which was meant.
fn die_ambiguous(arg: &str) -> ExitCode {
    eprintln!(
        "fatal: ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    );
    ExitCode::from(128)
}

/// `verify_non_filename()` (setup.c:297-308): the argument resolved as a revision
/// but also names something on disk.
fn die_both(arg: &str) -> ExitCode {
    eprintln!(
        "fatal: ambiguous argument '{arg}': both revision and filename\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    );
    ExitCode::from(128)
}

/// One `rev.pending` entry: the object the argument named (a tag stays a tag) and
/// whether it carries `UNINTERESTING | BOTTOM`. The flags live on the object, so
/// [`last_modified`] folds same-object entries together the way `obj->flags` does.
struct Tip {
    /// `obj->name`, which `populate_paths_from_revs()` quotes in its refusal.
    name: String,
    id: ObjectId,
    exclude: bool,
}

/// `verify_filename(..., diagnose_misspelt_rev = 0)`: an earlier argument was
/// already taken as a path, so this one can only be a path — and it is missing.
fn die_no_such_path(arg: &str) -> ExitCode {
    eprintln!(
        "fatal: {arg}: no such path in the working tree.\n\
         Use 'git <command> -- <path>...' to specify paths that do not exist locally."
    );
    ExitCode::from(128)
}

/// `check_filename()` (setup.c): whether the argument names something on disk, after
/// `:/` (a path from the work-tree root) and `:!` / `:^` (an exclude) are taken off. A bare
/// `:/`, `:!` or `:^` is "always exists". `lstat`, so a dangling symlink counts.
fn check_filename(repo: &gix::Repository, arg: &str) -> bool {
    let path = if let Some(rest) = arg.strip_prefix(":/") {
        if rest.is_empty() {
            return true;
        }
        let root = repo.workdir().unwrap_or_else(|| std::path::Path::new("."));
        return std::fs::symlink_metadata(root.join(rest)).is_ok();
    } else if let Some(rest) = arg.strip_prefix(":!").or_else(|| arg.strip_prefix(":^")) {
        if rest.is_empty() {
            return true;
        }
        rest
    } else {
        arg
    };
    std::fs::symlink_metadata(path).is_ok()
}

/// `--max-depth`'s value through `OPTION_INTEGER` (`precision = sizeof(int)`),
/// or the refusal: `error: option `max-depth' expects …` and exit 129, with no
/// usage block.
fn max_depth_value(v: &str) -> std::result::Result<i32, ExitCode> {
    match crate::optint::integer(&crate::optint::long_opt("max-depth"), v) {
        Ok(n) => Ok(n as i32),
        Err(e) => {
            eprintln!("error: {}", e.message());
            Err(ExitCode::from(129))
        }
    }
}

/// Parsed command line, mirroring `struct last_modified` plus the diff options
/// `last_modified_init()` sets on `rev.diffopt`.
struct Opts {
    /// `rev.diffopt.max_depth`; `max_depth_valid` is `max_depth >= 0`.
    max_depth: i32,
    /// `rev.diffopt.flags.tree_in_recursive`.
    show_trees: bool,
    /// `-z`.
    nul: bool,
    /// Pathspecs exactly as git keeps them in `pathspec.items[].match`
    /// (cwd prefix applied, trailing slashes preserved), sorted like
    /// `parse_pathspec` sorts them.
    pathspecs: Vec<BString>,
    /// The `:(exclude)` items, which `tree_entry_interesting()` evaluates in a second pass.
    excludes: Vec<BString>,
    /// Every item in argument order, the implicit "match everything" one last when all were
    /// excludes: what `check_recursion_depth()` walks.
    depth_items: Vec<BString>,
}

/// `git last-modified` — see the module docs for the covered surface.
pub fn last_modified(args: &[String]) -> Result<ExitCode> {
    // `parse_short_opt()`'s character loop (parse-options.c:426-461), so `-rt`
    // is `-r -t`. `-r`, `-t`, `-z` and `-h` are the whole short table and none
    // of them takes a value.
    let expanded = crate::parseopt::expand_short(args, crate::parseopt::Shorts::flags("rtzh"));
    let args = &expanded[..];

    let mut max_depth: i32 = 0;
    let mut show_trees = false;
    let mut nul = false;
    // What `parse_options()` hands `setup_revisions()`: every word ahead of `--` that is
    // not one of this command's options, in order. `PARSE_OPT_KEEP_UNKNOWN_OPT` keeps an
    // unrecognised option where it stood, so `-n 3`, `--not` and `--all` reach the revision
    // walk's own option table between the revisions they apply to.
    let mut words: Vec<&str> = Vec::new();
    // `PARSE_OPT_KEEP_DASHDASH` makes `parse_options` stop at `--`, so nothing past it is
    // read as an option, and `setup_revisions()` takes all of it as pathspecs.
    let mut after_dashdash: Vec<&str> = Vec::new();
    let mut seen_dashdash = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if seen_dashdash {
            after_dashdash.push(a);
            i += 1;
            continue;
        }
        match a {
            // `--help-all` joins `-h`: parse_options_step() tests that name with
            // a `strcmp()` of its own, ahead of parse_long_opt(), and renders
            // `USAGE_FULL` — identical here because this option table has no
            // `PARSE_OPT_HIDDEN` entry. The compare is exact, so `--help-a` and
            // `--help-all=x` fall through to the unknown-argument report.
            "-h" | "--help-all" => {
                // `parse_options` writes the usage to stdout for an explicit
                // `-h` and exits 0 with nothing on stderr.
                return Ok(super::show_usage(USAGE));
            }
            "--" => seen_dashdash = true,
            "-r" | "--recursive" => max_depth = -1,
            "--no-recursive" => max_depth = 0,
            "-t" | "--show-trees" => show_trees = true,
            "--no-show-trees" => show_trees = false,
            "-z" => nul = true,
            // `OPT_INTEGER_F` into a C `int` (builtin/last-modified.c:535):
            // `OPTION_INTEGER`'s diagnostics (parse-options.c:260-288), which
            // `parse_options` turns into a bare `error:` line and exit 129.
            "--max-depth" => {
                i += 1;
                let v = super::value_at(args, i, a)?;
                match max_depth_value(v) {
                    Ok(n) => max_depth = n,
                    Err(code) => return Ok(code),
                }
            }
            _ if a.starts_with("--max-depth=") => {
                match max_depth_value(&a["--max-depth=".len()..]) {
                    Ok(n) => max_depth = n,
                    Err(code) => return Ok(code),
                }
            }
            _ => words.push(a),
        }
        i += 1;
    }

    let repo = crate::setup::discover()?;
    // `repo_config(git_default_config)` runs once the options are parsed. The settings block is
    // lazy: `prepare_repo_settings()` first runs when the first revision resolves, so a bad
    // `core.packedGitLimit` loses to every error `setup_revisions()` raises before then.
    crate::default_config::validate(&repo).map_err(crate::default_config::Rejection::into_error)?;

    // `struct prio_queue queue = { compare_commits_by_gen_then_commit_date }`
    // (builtin/last-modified.c:347). That comparator (commit.c:909) sorts by
    // `commit_graph_generation()` first and only falls back to the commit date when the
    // two generations are equal — and with no commit-graph *every* generation is
    // `GENERATION_NUMBER_INFINITY` (commit-graph.c:134), so the fallback is the whole
    // comparator. With one, the numbers are real and the order can differ; the walk
    // below detects that case rather than refusing outright. See [`PrioQueue`].
    // `commit_graph_generation()` (commit-graph.c:126) answers `GENERATION_NUMBER_INFINITY`
    // for a commit that is not in the graph, the corrected commit date for one that is when
    // the whole chain carries `GDA2`, and the topological level otherwise
    // (commit-graph.c:902-917). Holding the graph open from `setup_revisions()` on gives all
    // three, and `A...B`'s merge-base walk reads the same numbers.
    let commit_graph = repo.commit_graph_if_enabled()?;

    // `setup_revisions()` (revision.c:3045-3132): every word is offered to the revision
    // walk's option table first, then to `handle_revision_arg()`; from the first word that
    // is neither, every word is a pathspec.
    let mut setup = Pending {
        repo: &repo,
        parsed: Parsed {
            graph: commit_graph.as_ref(),
            info: HashMap::new(),
        },
        tips: Vec::new(),
        settings_loaded: false,
    };
    // `revs->max_count` and the rest of `handle_revision_opt()`'s count-and-age arm
    // (revision.c:2373-2425). Only `max_count` is read by `last_modified_run()`.
    let mut counts = crate::revopt::Counts::default();
    // `flags` as `--not` toggles it (revision.c:2944-2945): whether a revision is pended
    // `UNINTERESTING | BOTTOM`.
    let mut not = false;
    // `revs->rev_input_given`: a revision or a ref-selecting option was seen, even one that
    // pended nothing, so `rev.def` stays out (revision.c:1651, 2262-2263, 3163).
    let mut rev_input_given = false;
    // `revs->pending` names the first unrecognised option `left` kept in argv;
    // `last_modified_init()` reports it after `setup_revisions()` returns.
    let mut unknown: Option<&str> = None;
    let mut specs: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let p = words[i];
        // `if (!seen_end_of_options && *arg == '-')` (revision.c:3080): a lone `-` is an
        // option word here too, and nothing claims it.
        if p.starts_with('-') {
            match p {
                // handle_revision_pseudo_opt() (revision.c:2944-2945).
                "--not" => not = !not,
                // handle_revision_pseudo_opt() (revision.c:2846-2855): every ref, then HEAD,
                // then the other worktrees' HEADs, each pended under `flags`.
                "--all" => {
                    rev_input_given = true;
                    match all_refs(&repo)? {
                        Ok(refs) => {
                            if !refs.is_empty() {
                                setup.load_settings()?;
                            }
                            for (name, id) in refs {
                                setup.tips.push(Tip { name, id, exclude: not });
                            }
                        }
                        Err(name) => {
                            // get_reference()'s `die("bad object %s", name)` (revision.c:396).
                            eprintln!("fatal: bad object {name}");
                            return Ok(ExitCode::from(128));
                        }
                    }
                }
                _ => match counts.parse(&words, i) {
                    Some(Ok(hit)) => {
                        i += hit.consumed;
                        continue;
                    }
                    // `-n` with nothing after it is `error()` (revision.c:2403-2404), which
                    // `setup_revisions()` turns into `exit(128)`; every other refusal there is
                    // a `die()`.
                    Some(Err(message)) if message.starts_with('-') => {
                        eprintln!("error: {message}");
                        return Ok(ExitCode::from(128));
                    }
                    Some(Err(message)) => {
                        eprintln!("fatal: {message}");
                        return Ok(ExitCode::from(128));
                    }
                    None => {
                        unknown.get_or_insert(p);
                    }
                },
            }
            i += 1;
            continue;
        }
        match setup.handle_revision_arg(p, not, seen_dashdash)? {
            Ok(true) => {
                rev_input_given = true;
                i += 1;
                continue;
            }
            Ok(false) => {}
            Err(code) => return Ok(code),
        }
        // revision.c:3119-3132.
        if seen_dashdash || p.starts_with('^') {
            eprintln!("fatal: bad revision '{p}'");
            return Ok(ExitCode::from(128));
        }
        // `verify_filename()` (setup.c:280-290) for this word and every one after it: an
        // option there is refused outright, and a wildcard or `:(` item passes unstatted.
        for (j, &rest) in words[i..].iter().enumerate() {
            if rest.starts_with('-') {
                eprintln!("fatal: option '{rest}' must come before non-option arguments");
                return Ok(ExitCode::from(128));
            }
            if !(crate::setup::looks_like_pathspec(rest) || check_filename(&repo, rest)) {
                return Ok(if j > 0 {
                    die_no_such_path(rest)
                } else {
                    die_ambiguous(rest)
                });
            }
            specs.push(rest);
        }
        break;
    }
    specs.extend(after_dashdash);
    let max_count = counts.max_count;

    if let Some(msg) = crate::pathspec::first_outside_repository_fatal(&repo, &specs, gix::pathspec::Defaults::default()) {
        eprintln!("fatal: {msg}");
        return Ok(ExitCode::from(128));
    }
    let mut pathspecs: Vec<BString> = Vec::new();
    let mut excludes: Vec<BString> = Vec::new();
    let mut depth_items: Vec<BString> = Vec::new();
    for s in &specs {
        let Some((exclude, path)) = split_exclude_magic(s) else {
            anyhow::bail!("unsupported pathspec magic {s:?}");
        };
        match crate::pathspec::prefix_path(&repo, path.as_bytes().as_bstr()) {
            Ok(p) => {
                depth_items.push(p.clone());
                if exclude {
                    excludes.push(p);
                } else {
                    pathspecs.push(p);
                }
            }
            Err(msg) => {
                eprintln!("fatal: {s}: {msg}");
                return Ok(ExitCode::from(128));
            }
        }
    }
    // The implicit `HEAD` resolves after the pathspecs are parsed.
    setup.load_settings()?;
    // `parse_pathspec()`: when every item is an exclude, one positive item matching everything
    // is added.
    if pathspecs.is_empty() && !excludes.is_empty() {
        pathspecs.push(BString::default());
        depth_items.push(BString::default());
    }
    // `diff_setup_done()` runs at the end of `setup_revisions()`, before the unknown-argument
    // check: a depth limit and a wildcard pathspec exclude each other.
    if max_depth >= 0 && pathspecs.iter().chain(&excludes).any(|p| nowildcard_len(p) < p.len()) {
        eprintln!("fatal: max-depth cannot be used with wildcard pathspecs");
        return Ok(ExitCode::from(128));
    }

    if let Some(a) = unknown {
        eprint!("error: unknown last-modified argument: {a}\n{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let Pending { mut parsed, mut tips, .. } = setup;
    // `rev.def = "HEAD"` (builtin/last-modified.c:481) stands in only when no
    // revision was given at all, so `^X` alone leaves nothing interesting.
    if tips.is_empty() && !rev_input_given {
        match repo.rev_parse_single("HEAD") {
            Ok(id) => tips.push(Tip {
                name: "HEAD".into(),
                id: id.detach(),
                exclude: false,
            }),
            Err(_) => return Ok(die_ambiguous("HEAD")),
        }
    }
    // The flags sit on the object, not on the pending entry: in `A..A` both entries read
    // back UNINTERESTING (revision.c:2122-2123).
    let excluded: HashSet<ObjectId> = tips.iter().filter(|t| t.exclude).map(|t| t.id).collect();

    // `populate_paths_from_revs()` (builtin/last-modified.c:139-160): the one interesting
    // entry names the commit whose tree supplies the paths. The second one is refused
    // only once the first has been peeled.
    let mut start_tree: Option<ObjectId> = None;
    for t in tips.iter().filter(|t| !excluded.contains(&t.id)) {
        if start_tree.is_some() {
            eprintln!("error: last-modified can only operate on one commit at a time");
            return Ok(ExitCode::from(255));
        }
        let object = repo.find_object(t.id)?;
        let kind = object.kind;
        match object.peel_to_commit() {
            Ok(c) => start_tree = Some(c.tree_id()?.detach()),
            Err(_) => {
                eprintln!("error: revision argument '{}' is a {kind}, not a commit-ish", t.name);
                return Ok(ExitCode::from(255));
            }
        }
    }

    // git reads `core.quotePath` once, in its config callback, into the global
    // every `quote_c_style()` caller shares.
    crate::quote::init(&repo);

    let opts = Opts {
        max_depth,
        show_trees,
        nul,
        pathspecs,
        excludes,
        depth_items,
    };

    // `populate_paths_from_revs`: diff the empty tree against the target tree,
    // which enumerates every path at the requested granularity, each with the oid
    // `mark_path()` later holds a boundary commit's entry to.
    let mut listed: Vec<(BString, Option<ObjectId>)> = Vec::new();
    if let Some(tree) = start_tree {
        diff_trees(&repo, None, Some(tree), b"", &opts, &mut listed)?;
    }
    let target_oid: HashMap<BString, ObjectId> =
        listed.iter().filter_map(|(p, oid)| Some((p.clone(), (*oid)?))).collect();

    // `all_paths` takes its order from git's hashmap iteration, not the diff.
    let all_paths = hashmap_order(listed.into_iter().map(|(p, _)| p).collect());
    let n = all_paths.len();
    let index: HashMap<&BString, usize> = all_paths.iter().enumerate().map(|(k, p)| (p, k)).collect();

    let mut out = Vec::<u8>::new();
    if n == 0 {
        std::io::stdout().write_all(&out)?;
        return Ok(ExitCode::SUCCESS);
    }

    // `prepare_revision_walk()` (revision.c:4026-4050) under `no_walk`: each pending
    // entry peels to its commit, a tag handing its flags to what it points at
    // (revision.c:394-414), each commit is listed once, and the list is sorted
    // newest-first by commit date, stably.
    let mut commits: Vec<(ObjectId, i64)> = Vec::new();
    let mut bottom: HashSet<ObjectId> = HashSet::new();
    for t in &tips {
        let peeled = repo.find_object(t.id)?.peel_tags_to_end()?;
        if peeled.kind != gix::object::Kind::Commit {
            continue;
        }
        let cid = peeled.id;
        if excluded.contains(&t.id) {
            bottom.insert(cid);
        }
        if !commits.iter().any(|(id, _)| *id == cid) {
            commits.push((cid, repo.find_commit(cid)?.time()?.seconds));
        }
    }
    commits.sort_by(|a, b| b.1.cmp(&a.1));

    // The commits git has parsed so far, with what `commit_graph_generation()` and
    // `commit->date` read for them. Both queues compare live: a commit nobody parsed yet
    // reads as `GENERATION_NUMBER_INFINITY` and date 0, and it changes once something
    // parses it, even while it sits in a queue. `setup_revisions()` parsed every tip and
    // every commit its revision syntax stepped through, which for `A...B` includes the whole
    // merge-base walk, already recorded in `parsed`.
    for t in &tips {
        for id in commits_parsed_by_name(&repo, &t.name, t.id) {
            parsed.parse(&repo, id)?;
        }
    }
    for (cid, _) in &commits {
        parsed.parse(&repo, *cid)?;
    }

    // The walk. `active[c]` is the bitmap of paths still looking for their
    // last-modifying commit at `c`; `pending` is the live path hashmap. `parent1` and
    // `parent2` are the PARENT1 / PARENT2 object flags (builtin/last-modified.c:23-24).
    let mut active: HashMap<ObjectId, Vec<bool>> = HashMap::new();
    let mut parent1: HashSet<ObjectId> = HashSet::new();
    let mut parent2: HashSet<ObjectId> = HashSet::new();
    let mut pending: Vec<bool> = vec![true; n];
    let mut queue = PrioQueue::default();
    let mut not_queue = PrioQueue::default();

    // builtin/last-modified.c:364-386: a BOTTOM commit seeds the `--not` queue, any
    // other the walk queue with every path active.
    for (cid, _) in commits {
        if bottom.contains(&cid) {
            not_queue.put(cid, &parsed);
            parent2.insert(cid);
        } else if parent1.insert(cid) {
            queue.put(cid, &parsed);
            active.insert(cid, vec![true; n]);
        }
    }

    let mut queue_popped: usize = 0;
    while let Some(c) = queue.get(&parsed) {
        let mut active_c = active.remove(&c).unwrap_or_else(|| vec![false; n]);
        let commit = repo.find_commit(c)?;
        let c_tree = commit.tree_id()?.detach();

        // `(0 <= max_count && max_count < ++queue_popped) || (c->object.flags & PARENT2)`
        // (builtin/last-modified.c:394-395): `-n` makes every commit popped past the limit
        // a boundary too, `++queue_popped` counting only while a limit is set.
        let over_limit = max_count.is_some_and(|max| {
            queue_popped += 1;
            max < queue_popped
        });
        if over_limit || parent2.contains(&c) {
            // A boundary commit (builtin/last-modified.c:396-407): it is marked
            // `PARENT2 | BOUNDARY`, and every path whose entry here is the version of interest
            // is emitted `^`-prefixed, in tree-diff order.
            parent2.insert(c);
            let mut entries: Vec<(BString, Option<ObjectId>)> = Vec::new();
            diff_trees(&repo, None, Some(c_tree), b"", &opts, &mut entries)?;
            for (path, oid) in &entries {
                let Some(&k) = index.get(path) else { continue };
                if pending[k] && oid.as_ref() == target_oid.get(path) {
                    pending[k] = false;
                    emit(&mut out, &all_paths[k], &c, true, &opts);
                }
            }
            continue;
        }

        // builtin/last-modified.c:416-430: mark the `--not` side's ancestry PARENT2 until
        // it reaches below `c`'s generation. A parent goes in unparsed.
        while let Some(nc) = not_queue.get(&parsed) {
            parsed.parse(&repo, nc)?;
            for pid in repo.find_commit(nc)?.parent_ids() {
                let pid = pid.detach();
                if parent2.insert(pid) {
                    not_queue.put(pid, &parsed);
                }
            }
            if parsed.key(&nc).0 < parsed.key(&c).0 {
                break;
            }
        }

        let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
        for pid in parents {
            // `process_parent()` parses the parent before anything else
            // (builtin/last-modified.c:290-291).
            parsed.parse(&repo, pid)?;
            let p_tree = repo.find_commit(pid)?.tree_id()?.detach();

            // Paths whose entry differs between parent and `c` are *not*
            // TREESAME and stay with `c`; every other active path moves up.
            let mut changed: Vec<(BString, Option<ObjectId>)> = Vec::new();
            diff_trees(&repo, Some(p_tree), Some(c_tree), b"", &opts, &mut changed)?;
            let mut not_same = vec![false; n];
            for (path, _) in &changed {
                if let Some(&k) = index.get(path) {
                    not_same[k] = true;
                }
            }

            let ap = active.entry(pid).or_insert_with(|| vec![false; n]);
            for k in 0..n {
                if active_c[k] && !not_same[k] {
                    active_c[k] = false;
                    ap[k] = true;
                }
            }
            let parent_has_paths = ap.iter().any(|b| *b);

            if parent_has_paths && parent1.insert(pid) {
                queue.put(pid, &parsed);
            }
            if !parent1.contains(&pid) {
                active.remove(&pid);
            }

            if !active_c.iter().any(|b| *b) {
                break;
            }
        }

        // Whatever is still active was changed by `c`.
        for k in 0..n {
            if active_c[k] && pending[k] {
                pending[k] = false;
                emit(&mut out, &all_paths[k], &c, false, &opts);
            }
        }
    }

    if pending.iter().any(|p| *p) {
        // builtin/last-modified.c:467-468: a starting commit that is also a BOTTOM one (a
        // tag excluded while the commit it names is the tip) never enters the walk queue.
        // `BUG()` aborts with stdout's buffer unflushed.
        eprintln!("BUG: builtin/last-modified.c:468: paths remaining beyond boundary in last-modified");
        std::process::abort();
    }

    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

/// git's `commit_graph_generation()` (commit-graph.c:126) for `id`.
///
/// A commit outside the graph — or a graph position whose data cannot be read — is
/// `GENERATION_NUMBER_INFINITY`, which is also the answer for every commit when there is no
/// graph at all. Inside the graph the number is the corrected commit date from `GDA2`, and only
/// when *every* file in the chain carries that chunk (`validate_mixed_generation_chain()`,
/// commit-graph.c:524-543); a chain without it falls back to the topological level held in the
/// upper bits of `CDAT` (commit-graph.c:917).
fn generation_of(graph: Option<&gix::commitgraph::Graph>, id: &ObjectId) -> u64 {
    let Some(graph) = graph else {
        return u64::from(gix::commitgraph::GENERATION_NUMBER_INFINITY);
    };
    let Some(commit) = graph.commit_by_id(id) else {
        return u64::from(gix::commitgraph::GENERATION_NUMBER_INFINITY);
    };
    if graph.has_generation_data() {
        if let Some(corrected) = commit.corrected_commit_date() {
            return corrected;
        }
    }
    u64::from(commit.generation())
}
/// The parsed-commit state both queues read their keys from.
struct Parsed<'g> {
    graph: Option<&'g gix::commitgraph::Graph>,
    /// `(commit_graph_generation(), commit->date)` for each parsed commit.
    info: HashMap<ObjectId, (u64, i64)>,
}

impl Parsed<'_> {
    /// `repo_parse_commit()`: fill in the generation and the commit date.
    fn parse(&mut self, repo: &gix::Repository, id: ObjectId) -> Result<()> {
        if !self.info.contains_key(&id) {
            let date = repo.find_commit(id)?.time()?.seconds;
            self.info.insert(id, (generation_of(self.graph, &id), date));
        }
        Ok(())
    }

    /// What the comparator sees: an unparsed commit has no slab generation
    /// (commit-graph.c:126-135) and a zero `date`.
    fn key(&self, id: &ObjectId) -> (u64, i64) {
        self.info
            .get(id)
            .copied()
            .unwrap_or((u64::from(gix::commitgraph::GENERATION_NUMBER_INFINITY), 0))
    }
}

/// Port of `struct prio_queue` (prio-queue.c) with
/// `compare_commits_by_gen_then_commit_date` (commit.c:902-921): newest generation
/// first, then newest date, then insertion order. The heap layout, the lazy
/// `get_pending` pop and the root replacement in `prio_queue_put()` are kept, because
/// the keys are read live and an in-queue commit can change key when it is parsed.
#[derive(Default)]
struct PrioQueue {
    /// `(ctr, commit)`.
    array: Vec<(usize, ObjectId)>,
    insertion_ctr: usize,
    get_pending: bool,
}

impl PrioQueue {
    /// `compare()` (prio-queue.c:4-12).
    fn compare(&self, i: usize, j: usize, parsed: &Parsed<'_>) -> std::cmp::Ordering {
        let (gen_a, date_a) = parsed.key(&self.array[i].1);
        let (gen_b, date_b) = parsed.key(&self.array[j].1);
        gen_b
            .cmp(&gen_a)
            .then(date_b.cmp(&date_a))
            .then(self.array[i].0.cmp(&self.array[j].0))
    }

    /// `sift_down_root()` (prio-queue.c:39-55).
    fn sift_down_root(&mut self, parsed: &Parsed<'_>) {
        let nr = self.array.len();
        let mut ix = 0;
        while ix * 2 + 1 < nr {
            let mut child = ix * 2 + 1;
            if child + 1 < nr && self.compare(child, child + 1, parsed).is_ge() {
                child += 1;
            }
            if self.compare(ix, child, parsed).is_le() {
                break;
            }
            self.array.swap(child, ix);
            ix = child;
        }
    }

    /// `prio_queue_put()` (prio-queue.c:66-93).
    fn put(&mut self, id: ObjectId, parsed: &Parsed<'_>) {
        let ctr = self.insertion_ctr;
        self.insertion_ctr += 1;
        if self.get_pending {
            self.get_pending = false;
            self.array[0] = (ctr, id);
            self.sift_down_root(parsed);
            return;
        }
        self.array.push((ctr, id));
        let mut ix = self.array.len() - 1;
        while ix > 0 {
            let parent = (ix - 1) / 2;
            if self.compare(parent, ix, parsed).is_le() {
                break;
            }
            self.array.swap(parent, ix);
            ix = parent;
        }
    }

    /// `prio_queue_get()` (prio-queue.c:95-108) with `flush_get()` (prio-queue.c:57-64).
    fn get(&mut self, parsed: &Parsed<'_>) -> Option<ObjectId> {
        if self.array.len() <= usize::from(self.get_pending) {
            self.array.clear();
            self.get_pending = false;
            return None;
        }
        if self.get_pending {
            self.array.swap_remove(0);
            self.sift_down_root(parsed);
        }
        self.get_pending = true;
        Some(self.array[0].1)
    }
}

/// `rev.pending` as `setup_revisions()` fills it, with the commits its revision syntax
/// parsed along the way.
struct Pending<'r, 'g> {
    repo: &'r gix::Repository,
    parsed: Parsed<'g>,
    tips: Vec<Tip>,
    settings_loaded: bool,
}

impl Pending<'_, '_> {
    /// `prepare_repo_settings()`, which the first revision lookup runs.
    fn load_settings(&mut self) -> Result<()> {
        if !self.settings_loaded {
            self.settings_loaded = true;
            crate::repo_settings::RepoSettings::load(self.repo).map_err(crate::fatal::die)?;
        }
        Ok(())
    }

    /// `handle_revision_arg_1()` (revision.c:2159-2259) under `flags` as `--not` left them:
    /// `Ok(true)` when the word pended something (or named a commit with nothing to pend),
    /// `Ok(false)` when it is no revision, `Err` for the `die()`s on the way.
    fn handle_revision_arg(
        &mut self,
        arg_: &str,
        not: bool,
        cant_be_filename: bool,
    ) -> Result<std::result::Result<bool, ExitCode>> {
        let repo = self.repo;
        // revision.c:2176-2183: a bare `..` is the parent-directory pathspec.
        if !cant_be_filename && arg_ == ".." {
            return Ok(Ok(false));
        }
        // handle_dotdot() (revision.c:2131-2157), tried first.
        if let Some(at) = arg_.find("..") {
            let a_name = &arg_[..at];
            let mut b_name = &arg_[at + 2..];
            let symmetric = b_name.starts_with('.');
            if symmetric {
                b_name = &b_name[1..];
            }
            let a_name = if a_name.is_empty() { "HEAD" } else { a_name };
            let b_name = if b_name.is_empty() { "HEAD" } else { b_name };
            if let (Ok(a_id), Ok(b_id)) = (repo.rev_parse_single(a_name), repo.rev_parse_single(b_name)) {
                let (a_id, b_id) = (a_id.detach(), b_id.detach());
                self.load_settings()?;
                // `handle_dotdot_1()`'s `verify_non_filename(full_name)` (revision.c:2086-2088).
                if !cant_be_filename && check_filename(repo, arg_) {
                    return Ok(Err(die_both(arg_)));
                }
                if symmetric {
                    // `A...B` (revision.c:2100-2120): both sides must be commits, their merge
                    // bases are pended first under `flags ^ (UNINTERESTING | BOTTOM)`, and A
                    // and B themselves under the plain flags.
                    let a = self.lookup_commit_reference(a_id)?;
                    let b = self.lookup_commit_reference(b_id)?;
                    let (Some(a), Some(b)) = (a, b) else {
                        // dotdot_missing() (revision.c:2054-2061).
                        eprintln!("fatal: Invalid symmetric difference expression {arg_}");
                        return Ok(Err(ExitCode::from(128)));
                    };
                    for base in self.get_merge_bases(a, b)? {
                        // add_pending_commit_list() names each by its hex id (revision.c:1987).
                        self.tips.push(Tip { name: base.to_string(), id: base, exclude: !not });
                    }
                    self.tips.push(Tip { name: a_name.to_owned(), id: a_id, exclude: not });
                } else {
                    // `A..B`: A carries `flags ^ (UNINTERESTING | BOTTOM)`, B the plain flags,
                    // and both are queued A first (revision.c:2095-2127).
                    self.tips.push(Tip { name: a_name.to_owned(), id: a_id, exclude: !not });
                }
                self.tips.push(Tip { name: b_name.to_owned(), id: b_id, exclude: not });
                return Ok(Ok(true));
            }
        }

        let mut arg = arg_;
        // `<rev>^@` (revision.c:2190-2197): the parents alone, under the plain flags.
        if let Some(mark) = arg.find("^@") {
            if mark + 2 == arg.len() && self.add_parents_only(&arg[..mark], not, 0)? {
                return Ok(Ok(true));
            }
        }
        // `<rev>^!` (revision.c:2198-2203): the parents excluded, then `<rev>` itself.
        if let Some(mark) = arg.find("^!") {
            if mark + 2 == arg.len() && self.add_parents_only(&arg[..mark], !not, 0)? {
                arg = &arg[..mark];
            }
        }
        // `<rev>^-[<n>]` (revision.c:2204-2219): parent `<n>` (default 1) excluded, then
        // `<rev>` itself. A count `strtol_i()` refuses, or one below 1, is no revision.
        if let Some(mark) = arg.find("^-") {
            let mut exclude_parent = 1;
            let count = &arg[mark + 2..];
            if !count.is_empty() {
                match crate::revopt::strtol_i(count) {
                    Some(n) if n >= 1 => exclude_parent = n,
                    _ => return Ok(Ok(false)),
                }
            }
            if self.add_parents_only(&arg[..mark], !not, exclude_parent)? {
                arg = &arg[..mark];
            }
        }
        // `*arg == '^'` sets `UNINTERESTING | BOTTOM` and is stripped before the lookup
        // (revision.c:2221-2225); `verify_non_filename()` sees the stripped name.
        let (name, exclude) = match arg.strip_prefix('^') {
            Some(rest) => (rest, !not),
            None => (arg, not),
        };
        let Ok(id) = repo.rev_parse_single(name) else {
            return Ok(Ok(false));
        };
        self.load_settings()?;
        if !cant_be_filename && check_filename(repo, name) {
            return Ok(Err(die_both(name)));
        }
        self.tips.push(Tip { name: name.to_owned(), id: id.detach(), exclude });
        Ok(Ok(true))
    }

    /// `add_parents_only()` (revision.c:1895-1934): pend the parents of the commit `arg_`
    /// names — only parent `exclude_parent` when that is non-zero — each under `arg` (its
    /// `^` taken off) and `exclude` (flipped by that `^`). `false` when `arg_` is not a
    /// commit-ish or has no such parent; the caller then tries the rest of the syntax.
    fn add_parents_only(&mut self, arg_: &str, mut exclude: bool, exclude_parent: i32) -> Result<bool> {
        let repo = self.repo;
        let mut arg = arg_;
        if let Some(rest) = arg.strip_prefix('^') {
            exclude = !exclude;
            arg = rest;
        }
        // repo_get_oid_committish().
        let Ok(id) = repo.rev_parse_single(arg) else {
            return Ok(false);
        };
        let id = id.detach();
        self.load_settings()?;
        // get_reference()'s `parse_object()`, once per tag peeled.
        let Ok(object) = repo.find_object(id).and_then(|o| Ok(o.peel_tags_to_end()?)) else {
            return Ok(false);
        };
        if object.kind != gix::object::Kind::Commit {
            return Ok(false);
        }
        for c in commits_parsed_by_name(repo, arg, id) {
            self.parsed.parse(repo, c)?;
        }
        let parents: Vec<ObjectId> = repo.find_commit(object.id)?.parent_ids().map(|p| p.detach()).collect();
        let wanted = usize::try_from(exclude_parent).unwrap_or(0);
        if wanted > parents.len() {
            return Ok(false);
        }
        for (k, parent) in parents.into_iter().enumerate() {
            if wanted != 0 && k + 1 != wanted {
                continue;
            }
            self.tips.push(Tip { name: arg.to_owned(), id: parent, exclude });
        }
        Ok(true)
    }

    /// `lookup_commit_reference()`: the commit `id` peels to, or its `error()` naming what
    /// the peel stopped at (object.c:object_as_type()).
    fn lookup_commit_reference(&mut self, id: ObjectId) -> Result<Option<ObjectId>> {
        let object = self.repo.find_object(id)?.peel_tags_to_end()?;
        if object.kind != gix::object::Kind::Commit {
            eprintln!("error: object {} is a {}, not a commit", object.id, object.kind);
            return Ok(None);
        }
        self.parsed.parse(self.repo, object.id)?;
        Ok(Some(object.id))
    }

    fn generation(&self, id: &ObjectId) -> u64 {
        self.parsed.key(id).0
    }

    fn parents(&self, id: ObjectId) -> Result<Vec<ObjectId>> {
        Ok(self.repo.find_commit(id)?.parent_ids().map(|p| p.detach()).collect())
    }

    /// `commit_list_sort_by_date()`: newest first, stable.
    fn sort_by_date(&self, list: &mut [ObjectId]) {
        list.sort_by(|a, b| self.parsed.key(b).1.cmp(&self.parsed.key(a).1));
    }

    /// `repo_get_merge_bases(one, two)`, i.e. `get_merge_bases_many_0()` with one "two",
    /// cleanup and `MERGE_BASE_FIND_ALL` (commit-reach.c:564-606). Every commit the walk
    /// parses is recorded, because the last-modified queues read those commits' dates.
    fn get_merge_bases(&mut self, one: ObjectId, two: ObjectId) -> Result<Vec<ObjectId>> {
        // merge_bases_many() (commit-reach.c:265-307).
        if one == two {
            return Ok(vec![one]);
        }
        self.parsed.parse(self.repo, one)?;
        self.parsed.parse(self.repo, two)?;
        let mut marks: HashMap<ObjectId, u8> = HashMap::new();
        let list = self.paint_down_to_common(one, &[two], 0, &mut marks)?;
        let mut result: Vec<ObjectId> =
            list.into_iter().filter(|c| marks.get(c).copied().unwrap_or(0) & MB_STALE == 0).collect();
        self.sort_by_date(&mut result);
        if result.len() <= 1 {
            return Ok(result);
        }
        let mut result = self.remove_redundant(result)?;
        self.sort_by_date(&mut result);
        Ok(result)
    }

    /// `paint_down_to_common()` under `MERGE_BASE_FIND_ALL` (commit-reach.c:186-263), with
    /// the counted queue of `struct paint_state` (commit-reach.c:84-184). `marks` holds the
    /// object flags it paints, which every caller clears afterwards.
    fn paint_down_to_common(
        &mut self,
        one: ObjectId,
        twos: &[ObjectId],
        min_generation: u64,
        marks: &mut HashMap<ObjectId, u8>,
    ) -> Result<Vec<ObjectId>> {
        let mut state = PaintState {
            queue: PrioQueue::default(),
            counts: [0; 3],
            min_generation,
            last_gen: u64::from(gix::commitgraph::GENERATION_NUMBER_INFINITY),
            topo_ceiling: if corrected_commit_dates_enabled(self.parsed.graph) {
                u64::from(gix::commitgraph::GENERATION_NUMBER_INFINITY)
            } else {
                GENERATION_NUMBER_V1_MAX
            },
        };
        *marks.entry(one).or_default() |= MB_PARENT1;
        if twos.is_empty() {
            return Ok(vec![one]);
        }
        self.paint_queue_put(&mut state, marks, one, 0);
        for &two in twos {
            self.paint_queue_put(&mut state, marks, two, MB_PARENT2);
        }
        let mut result = Vec::new();
        while let Some(c) = self.paint_queue_get(&mut state, marks) {
            let mut flags = marks[&c] & (MB_PARENT1 | MB_PARENT2 | MB_STALE);
            if flags == MB_PARENT1 | MB_PARENT2 {
                if marks[&c] & MB_RESULT == 0 {
                    *marks.get_mut(&c).expect("painted") |= MB_RESULT;
                    result.push(c);
                }
                // Mark parents of a found merge stale.
                flags |= MB_STALE;
            }
            for p in self.parents(c)? {
                if marks.get(&p).copied().unwrap_or(0) & flags == flags {
                    continue;
                }
                self.parsed.parse(self.repo, p)?;
                self.paint_queue_put(&mut state, marks, p, flags);
            }
        }
        self.sort_by_date(&mut result);
        Ok(result)
    }

    /// `paint_queue_put()` (commit-reach.c:109-123).
    fn paint_queue_put(&self, state: &mut PaintState, marks: &mut HashMap<ObjectId, u8>, c: ObjectId, add: u8) {
        let flags = marks.entry(c).or_default();
        let old = *flags;
        *flags |= add;
        let new = *flags;
        if old & MB_ENQUEUED != 0 {
            state.update(old, -1);
            state.update(new, 1);
        } else {
            *flags |= MB_ENQUEUED;
            state.queue.put(c, &self.parsed);
            state.update(new, 1);
        }
    }

    /// `paint_queue_get()` (commit-reach.c:129-170).
    fn paint_queue_get(&self, state: &mut PaintState, marks: &mut HashMap<ObjectId, u8>) -> Option<ObjectId> {
        let c = state.queue.get(&self.parsed)?;
        let flags = marks.get_mut(&c).expect("queued commits are painted");
        *flags &= !MB_ENQUEUED;
        let generation = self.generation(&c);
        state.last_gen = generation;
        if generation < state.min_generation {
            return None;
        }
        let [parent1, parent2, candidates] = state.counts;
        if candidates == 0 {
            if parent1 == 0 && parent2 == 0 {
                return None;
            }
            if (parent1 == 0 || parent2 == 0) && generation < state.topo_ceiling {
                return None;
            }
        }
        state.update(*flags, -1);
        Some(c)
    }

    /// `remove_redundant()` (commit-reach.c:545-562).
    fn remove_redundant(&mut self, array: Vec<ObjectId>) -> Result<Vec<ObjectId>> {
        if generation_numbers_enabled(self.parsed.graph)
            && array
                .iter()
                .any(|c| self.generation(c) < u64::from(gix::commitgraph::GENERATION_NUMBER_INFINITY))
        {
            return self.remove_redundant_with_gen(array);
        }
        self.remove_redundant_no_gen(array)
    }

    /// `remove_redundant_no_gen()` (commit-reach.c:368-427).
    fn remove_redundant_no_gen(&mut self, array: Vec<ObjectId>) -> Result<Vec<ObjectId>> {
        for &c in &array {
            self.parsed.parse(self.repo, c)?;
        }
        let mut redundant = vec![false; array.len()];
        for i in 0..array.len() {
            if redundant[i] {
                continue;
            }
            let mut min_generation = self.generation(&array[i]);
            let mut work = Vec::new();
            let mut filled_index = Vec::new();
            for j in 0..array.len() {
                if i == j || redundant[j] {
                    continue;
                }
                filled_index.push(j);
                work.push(array[j]);
                min_generation = min_generation.min(self.generation(&array[j]));
            }
            let mut marks: HashMap<ObjectId, u8> = HashMap::new();
            self.paint_down_to_common(array[i], &work, min_generation, &mut marks)?;
            let flag = |c: &ObjectId| marks.get(c).copied().unwrap_or(0);
            if flag(&array[i]) & MB_PARENT2 != 0 {
                redundant[i] = true;
            }
            for (k, w) in work.iter().enumerate() {
                if flag(w) & MB_PARENT1 != 0 {
                    redundant[filled_index[k]] = true;
                }
            }
        }
        Ok(array.into_iter().zip(redundant).filter(|(_, r)| !r).map(|(c, _)| c).collect())
    }

    /// `remove_redundant_with_gen()` (commit-reach.c:429-543).
    fn remove_redundant_with_gen(&mut self, array: Vec<ObjectId>) -> Result<Vec<ObjectId>> {
        let repo = self.repo;
        let mut marks: HashMap<ObjectId, u8> = HashMap::new();
        let flag = |marks: &HashMap<ObjectId, u8>, c: &ObjectId| marks.get(c).copied().unwrap_or(0);
        // Sort the input by generation number, ascending (`QSORT(compare_commits_by_gen)`).
        let mut sorted = array.clone();
        sorted.sort_by_key(|c| self.generation(c));
        let mut min_generation = self.generation(&sorted[0]);
        let mut min_gen_pos = 0;

        // Mark all parents of the input as STALE.
        let mut walk_start = Vec::new();
        for &c in &array {
            self.parsed.parse(repo, c)?;
            *marks.entry(c).or_default() |= MB_RESULT;
            for p in self.parents(c)? {
                self.parsed.parse(repo, p)?;
                if flag(&marks, &p) & MB_STALE == 0 {
                    *marks.entry(p).or_default() |= MB_STALE;
                    walk_start.push(p);
                }
            }
        }
        walk_start.sort_by_key(|c| self.generation(c));
        // Remove the STALE bit for now to allow walking through parents.
        for c in &walk_start {
            *marks.entry(*c).or_default() &= !MB_STALE;
        }

        // Start walking from the highest generation.
        let mut count_still_independent = array.len();
        for start in walk_start.iter().rev() {
            if count_still_independent <= 1 {
                break;
            }
            // Push the STALE bits up to min generation; the stack's head is its last element.
            let mut stack = vec![*start];
            *marks.entry(*start).or_default() |= MB_STALE;
            while let Some(&c) = stack.last() {
                self.parsed.parse(repo, c)?;
                if flag(&marks, &c) & MB_RESULT != 0 {
                    *marks.entry(c).or_default() &= !MB_RESULT;
                    count_still_independent -= 1;
                    if count_still_independent <= 1 {
                        break;
                    }
                    if c == sorted[min_gen_pos] {
                        while min_gen_pos < sorted.len() - 1 && flag(&marks, &sorted[min_gen_pos]) & MB_STALE != 0 {
                            min_gen_pos += 1;
                        }
                        min_generation = self.generation(&sorted[min_gen_pos]);
                    }
                }
                if self.generation(&c) < min_generation {
                    stack.pop();
                    continue;
                }
                let next = self.parents(c)?.into_iter().find(|p| flag(&marks, p) & MB_STALE == 0);
                match next {
                    Some(p) => {
                        *marks.entry(p).or_default() |= MB_STALE;
                        stack.push(p);
                    }
                    // Pop if all parents have been visited already.
                    None => {
                        stack.pop();
                    }
                }
            }
        }
        Ok(array.into_iter().filter(|c| flag(&marks, c) & MB_STALE == 0).collect())
    }
}

/// commit-reach.c's object flags (object.h: PARENT1, PARENT2, STALE, RESULT and the
/// paint queue's ENQUEUED), kept apart from the last-modified walk's own PARENT1/PARENT2
/// because every merge-base caller clears them before that walk starts.
const MB_PARENT1: u8 = 1 << 0;
const MB_PARENT2: u8 = 1 << 1;
const MB_STALE: u8 = 1 << 2;
const MB_RESULT: u8 = 1 << 3;
const MB_ENQUEUED: u8 = 1 << 4;

/// `GENERATION_NUMBER_V1_MAX` (commit.h:13).
const GENERATION_NUMBER_V1_MAX: u64 = 0x3FFF_FFFF;

/// `struct paint_state` (commit-reach.c:84-92): the queue and its per-side counters.
struct PaintState {
    queue: PrioQueue,
    /// `parent1_count`, `parent2_count`, `mb_candidate_count`.
    counts: [isize; 3],
    min_generation: u64,
    last_gen: u64,
    topo_ceiling: u64,
}

impl PaintState {
    /// `paint_count_update()` (commit-reach.c:94-107).
    fn update(&mut self, flags: u8, delta: isize) {
        match flags & (MB_PARENT1 | MB_PARENT2 | MB_STALE) {
            MB_PARENT1 => self.counts[0] += delta,
            MB_PARENT2 => self.counts[1] += delta,
            f if f == MB_PARENT1 | MB_PARENT2 => self.counts[2] += delta,
            _ => {}
        }
    }
}

/// `corrected_commit_dates_enabled()` (commit-graph.c:793-802).
fn corrected_commit_dates_enabled(graph: Option<&gix::commitgraph::Graph>) -> bool {
    graph.is_some_and(|g| g.num_commits() > 0 && g.has_generation_data())
}

/// `generation_numbers_enabled()` (commit-graph.c:778-791): the first commit's topological
/// level is non-zero. git reads it from the newest layer of a split chain; every layer is
/// written by the same `CDAT` code, so the base layer's first commit answers the same.
fn generation_numbers_enabled(graph: Option<&gix::commitgraph::Graph>) -> bool {
    graph.is_some_and(|g| g.num_commits() > 0 && g.commit_at(gix::commitgraph::Position(0)).generation() != 0)
}

/// `--all`'s pending entries in `handle_refs()` order (revision.c:2846-2855): every ref
/// under `refs/` by name (`refs_for_each_ref()`), then `HEAD` (`refs_head_ref()`), then each
/// other worktree's `HEAD` (`other_head_refs()`, worktree.c:602-637). Each names the object
/// the ref holds, a tag left a tag. `Err` names a ref whose object cannot be read, which is
/// get_reference()'s `die("bad object %s")`.
fn all_refs(repo: &gix::Repository) -> Result<std::result::Result<Vec<(String, ObjectId)>, String>> {
    let mut named: Vec<(Vec<u8>, Option<ObjectId>)> = Vec::new();
    for r in repo.references()?.all()? {
        match crate::fatal::ref_walk_entry(r)? {
            crate::fatal::RefWalkEntry::Ref(mut r) => {
                let name = r.name().as_bstr().to_vec();
                // A dangling symbolic ref is dropped before `handle_one_ref()` sees it.
                if let Ok(id) = r.follow_to_object() {
                    named.push((name, Some(id.detach())));
                }
            }
            crate::fatal::RefWalkEntry::Broken(name) => named.push((name.into_bytes(), None)),
            crate::fatal::RefWalkEntry::Omitted => {}
        }
    }
    named.sort();
    let mut out = Vec::new();
    for (name, id) in named {
        let name = String::from_utf8_lossy(&name).into_owned();
        match id {
            Some(id) if repo.find_header(id).is_ok() => out.push((name, id)),
            _ => return Ok(Err(name)),
        }
    }
    // refs_head_ref(): an unborn HEAD pends nothing.
    if let Ok(head) = repo.head() {
        if let Some(id) = head.id() {
            out.push(("HEAD".to_owned(), id.detach()));
        }
    }
    // other_head_refs(): the main worktree's HEAD when this is a linked one, then every
    // linked worktree but this one.
    if repo.git_dir() != repo.common_dir() {
        if let Ok(mut r) = repo.find_reference("main-worktree/HEAD") {
            if let Ok(id) = r.follow_to_object() {
                out.push(("main-worktree/HEAD".to_owned(), id.detach()));
            }
        }
    }
    for proxy in repo.worktrees().unwrap_or_default() {
        let name = format!("worktrees/{}/HEAD", proxy.id());
        let Ok(wt) = proxy.into_repo_with_possibly_inaccessible_worktree() else {
            continue;
        };
        if wt.git_dir() == repo.git_dir() {
            continue;
        }
        if let Some(id) = wt.head().ok().and_then(|h| h.id()) {
            out.push((name, id.detach()));
        }
    }
    Ok(Ok(out))
}

/// The commits `get_oid_with_context()` parses while it resolves `name` to `id`: the
/// base of a `~<n>` / `^<n>` / `^{...}` chain and each commit the chain steps through
/// (`get_nth_ancestor()` and `get_parent()`, object-name.c, parse every commit they
/// leave), then `id` itself (`get_reference()`'s `parse_object()`).
fn commits_parsed_by_name(repo: &gix::Repository, name: &str, id: ObjectId) -> Vec<ObjectId> {
    let mut out = Vec::new();
    let peel = |id: ObjectId| -> Option<ObjectId> {
        let object = repo.find_object(id).ok()?.peel_tags_to_end().ok()?;
        (object.kind == gix::object::Kind::Commit).then_some(object.id)
    };
    if let Some(c) = peel(id) {
        out.push(c);
    }
    let Some((base, ops)) = split_navigation(name) else {
        return out;
    };
    let Some(mut cur) = repo.rev_parse_single(base).ok().and_then(|b| peel(b.detach())) else {
        return out;
    };
    let parents = |c: ObjectId| -> Vec<ObjectId> {
        repo.find_commit(c)
            .map(|c| c.parent_ids().map(|p| p.detach()).collect())
            .unwrap_or_default()
    };
    for op in ops {
        out.push(cur);
        let next = match op {
            Nav::Ancestor(n) => {
                let mut c = Some(cur);
                for _ in 0..n {
                    c = c.and_then(|x| {
                        out.push(x);
                        parents(x).first().copied()
                    });
                }
                c
            }
            Nav::Parent(0) => Some(cur),
            Nav::Parent(n) => parents(cur).get(n - 1).copied(),
            Nav::Peel => peel(cur),
        };
        match next {
            Some(c) => cur = c,
            None => return out,
        }
    }
    out
}

/// One step of a revision's trailing navigation suffix.
enum Nav {
    /// `~<n>`.
    Ancestor(usize),
    /// `^<n>`.
    Parent(usize),
    /// `^{...}`.
    Peel,
}

/// Split `name` into the base before its trailing `~<n>` / `^<n>` / `^{...}` chain and
/// that chain, or `None` when there is no such chain.
fn split_navigation(name: &str) -> Option<(&str, Vec<Nav>)> {
    let parse_ops = |mut s: &str| -> Option<Vec<Nav>> {
        let mut ops = Vec::new();
        while !s.is_empty() {
            let op = s.as_bytes()[0];
            s = &s[1..];
            if op == b'^' && s.starts_with('{') {
                let end = s.find('}')?;
                s = &s[end + 1..];
                ops.push(Nav::Peel);
                continue;
            }
            let digits = s.bytes().take_while(u8::is_ascii_digit).count();
            let n = if digits == 0 { 1 } else { s[..digits].parse().ok()? };
            s = &s[digits..];
            ops.push(match op {
                b'~' => Nav::Ancestor(n),
                b'^' => Nav::Parent(n),
                _ => return None,
            });
        }
        Some(ops)
    };
    name.char_indices()
        .filter(|&(i, c)| i > 0 && (c == '~' || c == '^'))
        .find_map(|(i, _)| Some((&name[..i], parse_ops(&name[i..])?)))
}

/// A tree entry snapshot, detached from the tree's buffer so we can recurse.
struct Ent {
    name: BString,
    mode: u16,
    is_tree: bool,
    oid: ObjectId,
}

/// Read the entries of `id` in tree order; an absent tree reads as empty.
fn read_tree(repo: &gix::Repository, id: Option<ObjectId>) -> Result<Vec<Ent>> {
    let Some(id) = id else {
        return Ok(Vec::new());
    };
    let tree = repo.find_object(id)?.peel_to_tree()?;
    let decoded = tree.decode()?;
    Ok(decoded
        .entries
        .iter()
        .map(|e| Ent {
            name: e.filename.to_owned(),
            mode: e.mode.value(),
            is_tree: e.mode.is_tree(),
            oid: e.oid.to_owned(),
        })
        .collect())
}

/// `base_name_compare`: names compare bytewise, with directories behaving as
/// though they carried a trailing `/`.
fn base_name_compare(a: &Ent, b: &Ent) -> std::cmp::Ordering {
    let common = a.name.len().min(b.name.len());
    let ord = a.name[..common].cmp(&b.name[..common]);
    if ord != std::cmp::Ordering::Equal {
        return ord;
    }
    let ca = a
        .name
        .get(common)
        .copied()
        .unwrap_or(if a.is_tree { b'/' } else { 0 });
    let cb = b
        .name
        .get(common)
        .copied()
        .unwrap_or(if b.is_tree { b'/' } else { 0 });
    ca.cmp(&cb)
}

/// Port of `ll_diff_tree_paths` for two trees: append the path of every entry
/// (with its `p->two->oid`, `None` for a deletion) that differs between `old` and `new`, honouring the pathspec, `--max-depth`
/// and `--show-trees` exactly as `emit_path()` does. `old = None` is git's
/// empty-tree diff, which lists everything.
fn diff_trees(
    repo: &gix::Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    base: &[u8],
    opts: &Opts,
    out: &mut Vec<(BString, Option<ObjectId>)>,
) -> Result<()> {
    let olds = read_tree(repo, old)?;
    let news = read_tree(repo, new)?;

    let (mut i, mut j) = (0usize, 0usize);
    while i < olds.len() || j < news.len() {
        let (o, nw) = match (olds.get(i), news.get(j)) {
            (Some(a), Some(b)) => match base_name_compare(a, b) {
                std::cmp::Ordering::Less => {
                    i += 1;
                    (Some(a), None)
                }
                std::cmp::Ordering::Greater => {
                    j += 1;
                    (None, Some(b))
                }
                std::cmp::Ordering::Equal => {
                    i += 1;
                    j += 1;
                    if a.mode == b.mode && a.oid == b.oid {
                        continue;
                    }
                    (Some(a), Some(b))
                }
            },
            (Some(a), None) => {
                i += 1;
                (Some(a), None)
            }
            (None, Some(b)) => {
                j += 1;
                (None, Some(b))
            }
            (None, None) => unreachable!(),
        };

        let e = nw.or(o).expect("at least one side present");
        let mut path = BString::from(base.to_vec());
        path.extend_from_slice(&e.name);

        if !interesting(&path, e.is_tree, opts) {
            continue;
        }

        let mut recurse = false;
        let mut emit_this = true;
        if e.is_tree && should_recurse(&path, opts) {
            recurse = true;
            emit_this = opts.show_trees;
        }
        if emit_this {
            out.push((path.clone(), nw.map(|x| x.oid)));
        }
        if recurse {
            let mut child_base = path;
            child_base.push(b'/');
            diff_trees(
                repo,
                o.filter(|x| x.is_tree).map(|x| x.oid),
                nw.filter(|x| x.is_tree).map(|x| x.oid),
                &child_base,
                opts,
                out,
            )?;
        }
    }
    Ok(())
}


/// `tree_entry_interesting()` (tree-walk.c) for plain pathspecs, wildcards included:
/// the entry named `path` is interesting when any pathspec item says so. The
/// recursive flag is always set for last-modified, so a directory that no item
/// rules out is kept for its children to be matched.
fn interesting(path: &BString, is_dir: bool, opts: &Opts) -> bool {
    if opts.pathspecs.is_empty() {
        return true;
    }
    let p = path.as_bytes();
    let (base, name) = p.split_at(p.iter().rposition(|&c| c == b'/').map_or(0, |i| i + 1));
    let positive = match_level(&opts.pathspecs, base, name, is_dir);
    if opts.excludes.is_empty() || positive == Level::No {
        return positive != Level::No;
    }
    // The table at the end of `tree_entry_interesting()`: what the exclude items make of an
    // entry the positive items accepted.
    let negative = match_level(&opts.excludes, base, name, is_dir);
    match (positive, negative) {
        (_, Level::No) => true,
        (_, Level::Some) if is_dir => true,
        _ => false,
    }
}

/// `enum interesting` as `do_match()` returns it, less the "never interesting" early exits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    No,
    Some,
    All,
}

/// `do_match()`: the first item (last to first) that has an opinion decides.
fn match_level(items: &[BString], base: &[u8], name: &[u8], is_dir: bool) -> Level {
    items
        .iter()
        .rev()
        .map(|item| item_interesting(item.as_bytes(), base, name, is_dir))
        .find(|level| *level != Level::No)
        .unwrap_or(Level::No)
}

/// `:^`/`:!`/`:(exclude)` magic taken off a pathspec element: `(is_exclude, rest)`. `None` for
/// any other magic, which this command does not model.
fn split_exclude_magic(spec: &str) -> Option<(bool, &str)> {
    let Some(magic) = spec.strip_prefix(':') else {
        return Some((false, spec));
    };
    if let Some(long) = magic.strip_prefix('(') {
        let (words, rest) = long.split_once(')')?;
        return (words == "exclude").then_some((true, rest));
    }
    let end = magic.find(|c| c != '!' && c != '^').unwrap_or(magic.len());
    if end == 0 {
        return None;
    }
    let rest = &magic[end..];
    Some((true, rest.strip_prefix(':').unwrap_or(rest)))
}

/// `nowildcard_len`: length of the leading part of a pathspec free of glob specials.
fn nowildcard_len(m: &[u8]) -> usize {
    m.iter()
        .position(|c| matches!(c, b'*' | b'?' | b'[' | b'\\'))
        .unwrap_or(m.len())
}

/// `git_fnmatch()` for a pathspec without `:(glob)`/`:(icase)`: the first `prefix`
/// bytes compare literally, the rest goes to `wildmatch()` with no `WM_PATHNAME`.
fn git_fnmatch(pattern: &[u8], string: &[u8], prefix: usize) -> bool {
    if prefix > pattern.len() || string.len() < prefix || pattern[..prefix] != string[..prefix] {
        return false;
    }
    gix::glob::wildmatch(
        pattern[prefix..].as_bstr(),
        string[prefix..].as_bstr(),
        gix::glob::wildmatch::Mode::empty(),
    )
}

/// One iteration of `do_match()`'s loop over the pathspec items. `base` is the
/// directory of the entry (empty or ending in `/`), `name` the entry itself.
fn item_interesting(m: &[u8], base: &[u8], name: &[u8], is_dir: bool) -> Level {
    let nowild = nowildcard_len(m);
    let wild = nowild < m.len();

    if base.len() >= m.len() {
        // match_dir_prefix(): `base` is `m` itself or below it, so everything under it matches.
        if base.starts_with(m) && (m.is_empty() || base.get(m.len()) == Some(&b'/') || m.last() == Some(&b'/')) {
            return Level::All;
        }
    } else if base.is_empty() || m.starts_with(base) {
        if match_entry(&m[base.len()..], name, is_dir) {
            return Level::Some;
        }
        // a directory no item rules out is kept so its children can be matched
        let hit = wild && (git_fnmatch(&m[base.len()..], name, nowild.saturating_sub(base.len())) || is_dir);
        return if hit { Level::Some } else { Level::No };
    }
    if !wild {
        return Level::No;
    }

    // match_wildcard_base(): the part of `base` inside the literal prefix must agree.
    if nowild > 0 && !base.is_empty() {
        if base.len() >= nowild {
            if base[..nowild] != m[..nowild] {
                return Level::No;
            }
        } else if !m.starts_with(base) {
            return Level::No;
        }
    }
    let mut full = base.to_vec();
    full.extend_from_slice(name);
    if git_fnmatch(m, &full, nowild) || is_dir {
        Level::Some
    } else {
        Level::No
    }
}

/// `match_entry()`: `m` (the pathspec below `base`) names the entry or a directory above it.
fn match_entry(m: &[u8], name: &[u8], is_dir: bool) -> bool {
    if name.len() > m.len() {
        return false;
    }
    if m.len() > name.len() && (m[name.len()] != b'/' || !is_dir) {
        return false;
    }
    m[..name.len()] == *name
}

/// `is_dir_prefix()`: true when `dir` is a leading directory of `path`.
fn is_dir_prefix(path: &[u8], dir: &[u8]) -> bool {
    path.len() >= dir.len()
        && path.starts_with(dir)
        && (path.len() == dir.len() || path[dir.len()] == b'/')
}

/// `should_recurse()`. `flags.recursive` is always set for last-modified, so
/// only `max_depth_valid` (`max_depth >= 0`) gates the depth check.
fn should_recurse(path: &BString, opts: &Opts) -> bool {
    if opts.max_depth < 0 {
        return true;
    }
    let items = if opts.excludes.is_empty() { &opts.pathspecs } else { &opts.depth_items };
    check_recursion_depth(path.as_bytes(), items, opts.max_depth)
}

/// Port of `check_recursion_depth()`: depth is measured from the end of the
/// longest matching pathspec, so `-- src/` already sits one level deep.
fn check_recursion_depth(name: &[u8], ps: &[BString], max_depth: i32) -> bool {
    if ps.is_empty() {
        return within_depth(name, 1, max_depth);
    }
    for item in ps.iter().rev() {
        let item = item.as_bytes();
        if name.len() >= item.len() {
            if !is_dir_prefix(name, item) {
                continue;
            }
            return within_depth(&name[item.len()..], 1, max_depth);
        }
        if is_dir_prefix(item, name) {
            return true;
        }
    }
    false
}

/// Port of `within_depth()`.
fn within_depth(name: &[u8], mut depth: i32, max_depth: i32) -> bool {
    for &c in name {
        if c != b'/' {
            continue;
        }
        depth += 1;
        if depth > max_depth {
            return false;
        }
    }
    depth <= max_depth
}

/// `strhash()` — FNV-1a as git spells it in `memhash()`.
fn strhash(s: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for &c in s {
        hash = hash.wrapping_mul(0x0100_0193) ^ u32::from(c);
    }
    hash
}

/// Reproduce `hashmap.c`'s iteration order for `lm->all_paths`: entries are
/// prepended to their bucket's chain, the table grows by 4x past an 80% load
/// factor (rehashing chain-head first), and iteration runs bucket 0..tablesize
/// following each chain from its head.
fn hashmap_order(paths: Vec<BString>) -> Vec<BString> {
    const INITIAL_SIZE: usize = 64;
    const RESIZE_BITS: u32 = 2;
    const LOAD_FACTOR: usize = 80;

    let mut tablesize = INITIAL_SIZE;
    let mut grow_at = tablesize * LOAD_FACTOR / 100;
    let mut table: Vec<VecDeque<(u32, BString)>> = vec![VecDeque::new(); tablesize];
    let mut size = 0usize;

    for p in paths {
        let h = strhash(p.as_bytes());
        table[(h as usize) & (tablesize - 1)].push_front((h, p));
        size += 1;
        if size > grow_at {
            let newsize = tablesize << RESIZE_BITS;
            let old = std::mem::replace(&mut table, vec![VecDeque::new(); newsize]);
            tablesize = newsize;
            grow_at = tablesize * LOAD_FACTOR / 100;
            for chain in old {
                for e in chain {
                    table[(e.0 as usize) & (tablesize - 1)].push_front(e);
                }
            }
        }
    }

    table
        .into_iter()
        .flat_map(|chain| chain.into_iter().map(|(_, p)| p))
        .collect()
}

/// `last_modified_emit()`: `^` for a BOUNDARY commit, then `<oid> TAB <path>` terminated by LF (path C-quoted
/// when needed) or by NUL under `-z` (never quoted).
fn emit(out: &mut Vec<u8>, path: &BString, commit: &ObjectId, boundary: bool, opts: &Opts) {
    if boundary {
        out.push(b'^');
    }
    out.extend_from_slice(commit.to_hex().to_string().as_bytes());
    out.push(b'\t');
    if opts.nul {
        out.extend_from_slice(path.as_bytes());
        out.push(0);
    } else {
        write_c_quoted(out, path.as_bytes());
        out.push(b'\n');
    }
}

/// `quote_c_style()`: the name verbatim unless some byte needs escaping, in which
/// case the whole name double-quoted with C escapes. The table and the
/// `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn write_c_quoted(out: &mut Vec<u8>, name: &[u8]) {
    out.extend_from_slice(&crate::quote::quoted_name_bytes(name));
}
