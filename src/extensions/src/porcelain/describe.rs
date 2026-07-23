use anyhow::Result;
use gix::bstr::{BStr, BString, ByteSlice};
use gix::commit::describe::SelectRef;
use gix::glob::wildmatch::Mode;
use gix::revision::plumbing::describe::{Options as DescribeOptions, Outcome};
use gix::ObjectId;
use std::borrow::Cow;
use std::fmt::Display;
use std::process::ExitCode;

/// `git describe` — name a commit from the tags (or refs) in its past.
///
/// Backed by gitoxide's `commit().describe()` platform (`gix_revision::describe`),
/// which implements git's candidate-walk algorithm. Output matches stock
/// `git describe`: a bare `<tag>` on an exact match, otherwise
/// `<tag>-<depth>-g<shorthash>`, with an optional `-dirty` suffix.
///
/// Supported forms:
///   * `git describe [<commit-ish>...]`  — default target is `HEAD`
///   * `--tags`                          — consider lightweight tags too
///   * `--all`                           — consider every ref (incl. branches),
///                                         printed with its `refs/`-stripped path
///                                         (`heads/main`, `tags/v1`) like git
///   * `--long`                          — always emit the long form
///   * `--always`                        — fall back to the abbreviated hash
///   * `--first-parent`                  — only follow the first parent
///   * `--dirty[=<mark>]`                — append `-dirty` (or `-<mark>`) if the worktree is dirty
///   * `--abbrev=<n>`                    — hex digits for the hash (`0` prints the tag only)
///   * `--candidates=<n>`                — number of candidate tags to weigh
///   * `--exact-match`                   — only print when a tag points directly at the commit
///   * `--match=<pattern>`               — only consider names matching the glob (repeatable)
///   * `--exclude=<pattern>`             — drop names matching the glob (repeatable)
///   * `--contains`                      — find the tag that comes *after* the commit
///                                         (git's `name-rev` reverse-ancestry walk)
///   * `git describe <blob>`             — name the commit that introduced the blob,
///                                         suffixed with `:<path>`
///   * `--broken[=<mark>]`               — accepted; the mark is only appended when the
///                                         worktree diff itself fails, which cannot happen here
///
/// `--match`/`--exclude` are ports of `builtin/describe.c:get_name()`: the candidate
/// `name_by_oid` map is built here from the repo's refs filtered by `wildmatch`, then
/// handed to the low-level `gix_revision::describe()` primitive directly (the higher
/// `SelectRef` platform builds the map itself and cannot be filtered). `--contains`
/// is a port of `builtin/name-rev.c` (git delegates `describe --contains` to
/// `name-rev --peel-tag --name-only --no-undefined`). The `<blob>` form ports
/// `describe_blob()`'s `--objects --in-commit-order --reverse` walk.
///
/// Failure paths follow git exactly (see `builtin/describe.c`): option errors
/// exit 129 with `error:` on stderr, every `fatal:` exits 128, and the four
/// distinct "cannot name this commit" messages are kept apart because they tell
/// the user different things.
pub fn describe(args: &[String]) -> Result<ExitCode> {
    let mut all = false;
    let mut tags = false;
    let mut long = false;
    let mut always = false;
    let mut first_parent = false;
    let mut contains = false;
    let mut max_candidates: usize = 10;
    // Raw `--abbrev` value, still signed: git clamps it late and treats 0 specially.
    let mut abbrev: Option<i64> = None;
    // Outer Option: was --dirty given? Inner Option: the custom mark, if any.
    let mut dirty: Option<Option<String>> = None;
    // OPT_STRING_LIST accumulators: patterns are OR-ed, excludes are AND-ed.
    let mut match_pats: Vec<BString> = Vec::new();
    let mut exclude_pats: Vec<BString> = Vec::new();
    let mut revs: Vec<&str> = Vec::new();

    let mut it = args.iter();
    let mut no_more_options = false;
    while let Some(arg) = it.next() {
        let a = arg.as_str();
        if no_more_options || !a.starts_with('-') || a == "-" {
            revs.push(a);
            continue;
        }
        match a {
            "--" => no_more_options = true,
            "--all" => all = true,
            "--no-all" => all = false,
            "--tags" => tags = true,
            "--no-tags" => tags = false,
            "--long" => long = true,
            "--no-long" => long = false,
            "--always" => always = true,
            "--no-always" => always = false,
            "--first-parent" => first_parent = true,
            "--no-first-parent" => first_parent = false,
            "--contains" => contains = true,
            "--no-contains" => contains = false,
            "--exact-match" => max_candidates = 0,
            "--no-exact-match" => max_candidates = 10,
            "--dirty" => dirty = Some(None),
            "--no-dirty" => dirty = None,
            // A healthy worktree never earns the broken mark, so the flag is inert here.
            "--broken" | "--no-broken" => {}
            // A bare `--abbrev` restores the repo-sized default; `--no-abbrev` is 0.
            "--abbrev" => abbrev = None,
            "--no-abbrev" => abbrev = Some(0),
            "--debug" | "--no-debug" => {}
            "--candidates" => match it.next() {
                Some(v) => match v.parse::<i64>() {
                    Ok(n) => max_candidates = n.max(0) as usize,
                    Err(_) => return integer_value_error("candidates"),
                },
                None => return missing_value_error("candidates"),
            },
            "--match" => match it.next() {
                Some(v) => match_pats.push(BString::from(v.as_str())),
                None => return missing_value_error("match"),
            },
            "--no-match" => match_pats.clear(),
            "--exclude" => match it.next() {
                Some(v) => exclude_pats.push(BString::from(v.as_str())),
                None => return missing_value_error("exclude"),
            },
            "--no-exclude" => exclude_pats.clear(),
            _ if a.starts_with("--dirty=") => dirty = Some(Some(a["--dirty=".len()..].to_string())),
            _ if a.starts_with("--broken=") => {}
            // git parses `--abbrev`'s value with C `strtol` into an `int`
            // (`parse_opt_abbrev_cb`): it errors only on missing digits or
            // trailing garbage, silently saturates on overflow, then truncates
            // the `long` result to 32 bits before its own clamp. Reject-on-error
            // matches git's 129; the truncated value flows through `hex_len`'s
            // clamp (negatives/1..3 -> 4, >hexsz -> hexsz) and the `--abbrev=0`
            // tag-only path exactly as git's post-strtol clamp does.
            _ if a.starts_with("--abbrev=") => match parse_c_int(&a["--abbrev=".len()..]) {
                Some(n) => abbrev = Some(n),
                None => return numerical_value_error("abbrev"),
            },
            _ if a.starts_with("--candidates=") => {
                match a["--candidates=".len()..].parse::<i64>() {
                    Ok(n) => max_candidates = n.max(0) as usize,
                    Err(_) => return integer_value_error("candidates"),
                }
            }
            _ if a.starts_with("--match=") => match_pats.push(BString::from(&a["--match=".len()..])),
            _ if a.starts_with("--exclude=") => {
                exclude_pats.push(BString::from(&a["--exclude=".len()..]))
            }
            _ => return unknown_option_error(a.trim_start_matches('-')),
        }
    }

    // git rejects this combination while still parsing, before touching the repo.
    if long && abbrev == Some(0) {
        return fatal("options '--long' and '--abbrev=0' cannot be used together");
    }

    // git's `--contains` short-circuits into name-rev *before* the dirty check and
    // the "No names found" gate, so those never apply to it (`builtin/describe.c`).
    if contains {
        return run_contains(&revs, all, always, &match_pats, &exclude_pats);
    }

    if dirty.is_some() && !revs.is_empty() {
        return fatal("option '--dirty' and commit-ishes cannot be used together");
    }

    // `--all` wins the ref selection; `--tags` only widens tags to unannotated ones.
    let select = if all {
        SelectRef::AllRefs
    } else if tags {
        SelectRef::AllTags
    } else {
        SelectRef::AnnotatedTags
    };
    let filter = Filter { all, match_pats, exclude_pats };

    // An object cache saves ~40% of walk time and is otherwise harmless.
    let mut repo = gix::discover(".")?;
    repo.object_cache_size_if_unset(4 * 1024 * 1024);

    // Resolve the dirty mark once; the check is over tracked changes only,
    // matching git (untracked files never make describe report `-dirty`).
    let dirty_mark = match &dirty {
        Some(mark) => {
            if repo.is_dirty()? {
                Some(mark.clone().unwrap_or_else(|| "dirty".to_string()))
            } else {
                None
            }
        }
        None => None,
    };

    // git's first bail: with nothing to name from and no hash fallback, stop before
    // walking. The selector decides what counts as a name — under the default only
    // tags are collected, but unannotated ones still count towards "some name exists".
    // With `--match`/`--exclude`, this is `names.nr` *after* filtering, so a pattern
    // that excludes every ref reproduces git's "No names found" (not the per-commit
    // "No tags can describe").
    if !always && !has_any_name(&repo, select, &filter)? {
        return fatal("No names found, cannot describe anything.");
    }

    let opts = Options {
        select,
        prefix_names: all,
        long,
        always,
        first_parent,
        max_candidates,
        abbrev,
        dirty_mark,
    };

    // Default target is HEAD; otherwise each positional commit-ish, in order.
    // git dies on the first one it cannot handle, so a failure stops the loop.
    if revs.is_empty() {
        let commit = repo.head_commit()?;
        if let Some(code) = describe_one(&repo, &commit, &opts, &filter)? {
            return Ok(code);
        }
    } else {
        for rev in &revs {
            let id = match repo.rev_parse_single(*rev) {
                Ok(id) => id,
                Err(_) => return fatal(format!("Not a valid object name {rev}")),
            };
            // git names a commit (tags peel through) directly; a blob is routed to
            // the tree-path search; anything else is fatal (`builtin/describe.c`).
            match id.object()?.peel_to_commit() {
                Ok(commit) => {
                    if let Some(code) = describe_one(&repo, &commit, &opts, &filter)? {
                        return Ok(code);
                    }
                }
                Err(_) => {
                    let obj = id.object()?;
                    if obj.kind == gix::object::Kind::Blob {
                        if let Some(code) = describe_blob(&repo, obj.id, &opts, &filter)? {
                            return Ok(code);
                        }
                    } else {
                        return fatal(format!("{rev} is neither a commit nor blob"));
                    }
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The `<blob>` form: a port of `builtin/describe.c:describe_blob()`.
///
/// git walks `HEAD`'s history with `--objects --in-commit-order --reverse` and names
/// the first (oldest) commit whose object set contains the blob, appending `:<path>`
/// — the blob's path in that commit's tree. Reproduced here by walking oldest-first
/// and scanning each commit's tree for the blob, then describing that commit with the
/// active options. An unreachable blob is git's fatal.
fn describe_blob(
    repo: &gix::Repository,
    blob: ObjectId,
    opts: &Options,
    filter: &Filter,
) -> Result<Option<ExitCode>> {
    let head = repo.head_commit()?;
    let walk = repo
        .rev_walk(Some(head.id))
        .sorting(gix::revision::walk::Sorting::ByCommitTime(
            gix::traverse::commit::simple::CommitTimeOrder::OldestFirst,
        ))
        .all()?;
    for info in walk {
        let oid = info?.id;
        let commit = repo.find_object(oid)?.try_into_commit()?;
        let entries = commit.tree()?.traverse().breadthfirst.files()?;
        if let Some(entry) = entries.into_iter().find(|e| e.oid == blob) {
            return match describe_commit_to_string(repo, &commit, opts, filter)? {
                Ok(s) => {
                    println!("{s}:{}", entry.filepath.to_str_lossy());
                    Ok(None)
                }
                Err(code) => Ok(Some(code)),
            };
        }
    }
    Ok(Some(fatal(format!("blob '{blob}' not reachable from HEAD"))?))
}

/// Everything the per-commit walk needs, resolved once for the whole invocation.
struct Options {
    select: SelectRef,
    /// `--all`: names print as their `refs/`-stripped path rather than shortened.
    prefix_names: bool,
    long: bool,
    always: bool,
    first_parent: bool,
    max_candidates: usize,
    abbrev: Option<i64>,
    dirty_mark: Option<String>,
}

/// `--match`/`--exclude` glob filter, a port of the accept-test in
/// `builtin/describe.c:get_name()`.
struct Filter {
    all: bool,
    match_pats: Vec<BString>,
    exclude_pats: Vec<BString>,
}

impl Filter {
    fn is_active(&self) -> bool {
        !self.match_pats.is_empty() || !self.exclude_pats.is_empty()
    }

    /// git's `get_name()` accept test, keyed on a full ref name. Only meaningful
    /// while `is_active()`; `path_to_match` is the ref's short form (`refs/tags/`,
    /// or under `--all` `refs/heads/`/`refs/remotes/`, stripped) exactly as git's
    /// `skip_prefix` chain computes it — including git's rule that with patterns
    /// present `--all` accepts only tags/heads/remotes.
    fn accepts(&self, full: &BStr) -> bool {
        let path_to_match: &BStr = if let Some(rest) = full.strip_prefix(b"refs/tags/") {
            rest.as_bstr()
        } else if self.all {
            if let Some(rest) = full.strip_prefix(b"refs/heads/") {
                rest.as_bstr()
            } else if let Some(rest) = full.strip_prefix(b"refs/remotes/") {
                rest.as_bstr()
            } else {
                return false;
            }
        } else {
            return false;
        };

        // Exclude wins first: any exclude glob that matches drops the ref.
        for pat in &self.exclude_pats {
            if gix::glob::wildmatch(pat.as_bstr(), path_to_match, Mode::empty()) {
                return false;
            }
        }
        // With match globs present, at least one must match.
        if !self.match_pats.is_empty() {
            let matched = self
                .match_pats
                .iter()
                .any(|pat| gix::glob::wildmatch(pat.as_bstr(), path_to_match, Mode::empty()));
            if !matched {
                return false;
            }
        }
        true
    }
}

/// Run the describe walk for a single already-resolved commit and print its name.
///
/// `Ok(None)` means the name was printed; `Ok(Some(code))` is git's fatal exit for
/// this commit, which stops the caller from describing any later commit-ish.
fn describe_one(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    opts: &Options,
    filter: &Filter,
) -> Result<Option<ExitCode>> {
    match describe_commit_to_string(repo, commit, opts, filter)? {
        Ok(s) => {
            println!("{s}");
            Ok(None)
        }
        Err(code) => Ok(Some(code)),
    }
}

/// Resolve and format a single commit into its `git describe` string, or return the
/// fatal exit code (message already emitted) for git's "cannot name this" cases.
///
/// Split out from [`describe_one`] so the `<blob>` form can reuse the exact commit
/// describe and append `:<path>` to it, matching `builtin/describe.c:describe_blob()`.
fn describe_commit_to_string(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    opts: &Options,
    filter: &Filter,
) -> Result<std::result::Result<String, ExitCode>> {
    let exact_only = opts.max_candidates == 0;
    // With --exact-match git dies rather than falling back to the hash, so the
    // fallback is withheld here and applied by hand below.
    let fallback = opts.always && !exact_only;
    let id = commit.id();
    let commit_oid = commit.id;

    // With a glob filter active the higher SelectRef platform can't be used (it
    // builds its own unfiltered name map); resolve through the low-level primitive
    // with a hand-built, filtered map instead. Otherwise use the platform as before.
    let outcome = if filter.is_active() {
        resolve_filtered(repo, &commit_oid, opts.select, filter, opts.max_candidates, fallback, opts.first_parent)?
    } else {
        commit
            .describe()
            .names(opts.select)
            .traverse_first_parent(opts.first_parent)
            .max_candidates(opts.max_candidates)
            .id_as_fallback(fallback)
            .try_resolve()?
            .map(|r| r.outcome)
    };

    let outcome = match outcome {
        Some(o) => o,
        None => {
            if exact_only {
                return Ok(Err(fatal(format!("no tag exactly matches '{commit_oid}'"))?));
            }
            // Distinguish "there are lightweight tags you are ignoring" from "there
            // is nothing reachable at all" the way git does: re-run the same walk
            // with unannotated tags admitted (still honoring the glob filter) and
            // see whether that finds a name.
            let unannotated_would_help = opts.select == SelectRef::AnnotatedTags && {
                let alt = if filter.is_active() {
                    resolve_filtered(
                        repo,
                        &commit_oid,
                        SelectRef::AllTags,
                        filter,
                        opts.max_candidates,
                        false,
                        opts.first_parent,
                    )?
                } else {
                    commit
                        .describe()
                        .names(SelectRef::AllTags)
                        .traverse_first_parent(opts.first_parent)
                        .max_candidates(opts.max_candidates)
                        .try_resolve()?
                        .map(|r| r.outcome)
                };
                alt.is_some()
            };
            let code = if unannotated_would_help {
                fatal(format!(
                    "No annotated tags can describe '{commit_oid}'.\nHowever, there were unannotated tags: try --tags."
                ))?
            } else {
                fatal(format!(
                    "No tags can describe '{commit_oid}'.\nTry --always, or create some tags."
                ))?
            };
            return Ok(Err(code));
        }
    };

    Ok(Ok(format_outcome(repo, outcome, &id, opts)?))
}

/// Resolve a commit through the low-level `gix_revision::describe()` primitive with
/// a caller-built candidate map, so `--match`/`--exclude` filtering can be applied
/// before the walk (the `SelectRef` platform builds the map internally, unfiltered).
fn resolve_filtered(
    repo: &gix::Repository,
    commit_oid: &ObjectId,
    select: SelectRef,
    filter: &Filter,
    max_candidates: usize,
    fallback: bool,
    first_parent: bool,
) -> Result<Option<Outcome<'static>>> {
    let name_by_oid = build_names(repo, select, filter)?;
    let cache = repo.commit_graph_if_enabled()?;
    let mut graph =
        repo.revision_graph::<gix::revision::plumbing::describe::Flags>(cache.as_ref());
    let outcome = gix::revision::plumbing::describe(
        commit_oid,
        &mut graph,
        DescribeOptions {
            name_by_oid,
            max_candidates,
            fallback_to_oid: fallback,
            first_parent,
        },
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(outcome)
}

/// Build the `name_by_oid` candidate map for `select`, dropping refs the glob filter
/// rejects. This mirrors gix's `SelectRef::names()` (the priority/time tie-breaks and
/// the "more recent wins on id collision" hashmap semantics are load-bearing) with
/// `Filter::accepts()` — git's `get_name()` accept test — spliced into the iteration.
fn build_names(
    repo: &gix::Repository,
    select: SelectRef,
    filter: &Filter,
) -> Result<gix::hashtable::HashMap<ObjectId, Cow<'static, BStr>>> {
    let platform = repo.references()?;
    let map = match select {
        SelectRef::AnnotatedTags => {
            // Only annotated tags become prio-2 candidates; lightweight tags fail
            // try_into_tag and drop out.
            let mut tags: Vec<(ObjectId, i64, Cow<'static, BStr>)> = platform
                .tags()?
                .filter_map(Result::ok)
                .filter_map(|r| {
                    if !filter.accepts(r.name().as_bstr()) {
                        return None;
                    }
                    let tag = r.try_id()?.object().ok()?.try_into_tag().ok()?;
                    let tag_time = tag.tagger().ok().and_then(|s| s.map(|s| s.seconds())).unwrap_or(0);
                    let commit_id = tag.target_id().ok()?.object().ok()?.try_into_commit().ok()?.id;
                    let name: Cow<'static, BStr> = Cow::Owned(r.name().shorten().to_owned());
                    Some((commit_id, tag_time, name))
                })
                .collect();
            // Sort by time ascending, then name descending; later entries overwrite
            // earlier ones when collected into the hashmap.
            tags.sort_by(|(_, a_time, a_name), (_, b_time, b_name)| {
                a_time.cmp(b_time).then_with(|| b_name.cmp(a_name))
            });
            tags.into_iter().map(|(id, _, name)| (id, name)).collect()
        }
        SelectRef::AllTags | SelectRef::AllRefs => {
            let mut refs: Vec<(ObjectId, u8, i64, Cow<'static, BStr>)> = match select {
                SelectRef::AllRefs => platform.all()?,
                _ => platform.tags()?,
            }
            .filter_map(Result::ok)
            .filter_map(|mut r| {
                if !filter.accepts(r.name().as_bstr()) {
                    return None;
                }
                let target_id = r.target().try_id().map(ToOwned::to_owned);
                let peeled = r.peel_to_id().ok()?.detach();
                let (prio, tag_time): (u8, i64) = match target_id {
                    Some(tid) if peeled != tid => {
                        let tag = repo.find_object(tid).ok()?.try_into_tag().ok()?;
                        let tag_time =
                            tag.tagger().ok().and_then(|s| s.map(|s| s.seconds())).unwrap_or(0);
                        (1, tag_time)
                    }
                    _ => (0, 0),
                };
                let name: Cow<'static, BStr> = Cow::Owned(r.name().shorten().to_owned());
                Some((peeled, prio, tag_time, name))
            })
            .collect();
            // By priority, then time ascending, then name descending; later entries
            // overwrite earlier ones on collection into the hashmap.
            refs.sort_by(|(_, a_prio, a_time, a_name), (_, b_prio, b_time, b_name)| {
                a_prio
                    .cmp(b_prio)
                    .then_with(|| a_time.cmp(b_time))
                    .then_with(|| b_name.cmp(a_name))
            });
            refs.into_iter().map(|(id, _, _, name)| (id, name)).collect()
        }
    };
    Ok(map)
}

/// Format an already-resolved outcome into git's describe string.
fn format_outcome(
    repo: &gix::Repository,
    mut outcome: Outcome<'static>,
    id: &gix::Id<'_>,
    opts: &Options,
) -> Result<String> {
    // gix shortens ref names (`refs/heads/main` -> `main`); `--all` wants git's
    // form, which only strips `refs/`. Rewrite before formatting so the long form
    // (`heads/main-2-gabc1234`) picks the prefixed name up as well.
    if opts.prefix_names {
        let prefixed = outcome
            .name
            .as_deref()
            .and_then(|name| prefixed_name(repo, name));
        if let Some(full) = prefixed {
            outcome.name = Some(Cow::Owned(full));
        }
    }

    // `--abbrev=0` is git's request to drop the `-<depth>-g<hash>` tail entirely.
    let mut out = if opts.abbrev == Some(0) {
        match &outcome.name {
            Some(name) => name.to_string(),
            // Only reachable via --always, where git prints the id in full.
            None => outcome.id.to_string(),
        }
    } else {
        let hex = hex_len(id, opts.abbrev)?;
        let mut format = outcome.into_format(hex);
        format.long(opts.long);
        format.to_string()
    };

    if let Some(mark) = &opts.dirty_mark {
        out.push('-');
        out.push_str(mark);
    }

    Ok(out)
}

/// git's abbreviation clamp: below `MINIMUM_ABBREV` it widens to 4, above the hash
/// width it saturates, and an absent value means "let the repo size decide".
fn hex_len(id: &gix::Id<'_>, abbrev: Option<i64>) -> Result<usize> {
    let full = id.kind().len_in_hex();
    Ok(match abbrev {
        Some(n) => (n.max(4) as usize).min(full),
        None => id.shorten()?.hex_len(),
    })
}

/// Does any ref the selector would collect (after glob filtering) exist at all?
///
/// This is git's `names.nr` test. Under the default selector git still collects
/// unannotated tags into `names` (they just lose the candidate contest later), so
/// a repo with only lightweight tags is *not* "no names found". With a glob filter,
/// only refs that pass `get_name()`'s accept test count, matching git's post-filter
/// `names.nr`.
fn has_any_name(repo: &gix::Repository, select: SelectRef, filter: &Filter) -> Result<bool> {
    let platform = repo.references()?;
    let mut iter = match select {
        SelectRef::AllRefs => platform.all()?,
        SelectRef::AllTags | SelectRef::AnnotatedTags => platform.tags()?,
    };
    if !filter.is_active() {
        return Ok(iter.next().is_some());
    }
    for r in iter.filter_map(Result::ok) {
        if filter.accepts(r.name().as_bstr()) {
            return Ok(true);
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------------------
// `--contains`: a port of `builtin/name-rev.c`.
//
// git implements `git describe --contains` by delegating to
// `name-rev --peel-tag --name-only --no-undefined [--always] [--tags
// --refs=refs/tags/<pat> --exclude=refs/tags/<pat>]`.  It seeds a table from every
// (filtered) ref tip, then walks each tip's ancestry backward assigning the tip's
// name to the commits it contains, keeping the best name per commit.  A commit is
// then named `<tip>`, `<tip>~<gen>`, `<tip>^<n>`, or `<tip>~<gen>^<n>`.
// ---------------------------------------------------------------------------

const MERGE_TRAVERSAL_WEIGHT: i32 = 65535;
const CUTOFF_DATE_SLOP: i64 = 86400;

/// A commit's best current name, mirroring name-rev's `struct rev_name`.
#[derive(Clone)]
struct RevName {
    tip_name: String,
    taggerdate: i64,
    generation: i32,
    distance: i32,
    from_tag: bool,
}

/// Cached parse of a commit: its committer time and parent ids.
#[derive(Clone)]
struct CommitInfo {
    date: i64,
    parents: Vec<ObjectId>,
}

/// A named ref tip feeding the reverse walk, mirroring `struct tip_table_entry`.
struct Tip {
    commit: ObjectId,
    refname: String,
    taggerdate: i64,
    from_tag: bool,
    deref: bool,
}

fn effective_distance(distance: i32, generation: i32) -> i32 {
    distance + if generation > 0 { MERGE_TRAVERSAL_WEIGHT } else { 0 }
}

/// name-rev's `is_better_name()`: prefer tags, then shorter effective distance,
/// then older tagger date.
fn is_better_name(cur: &RevName, taggerdate: i64, generation: i32, distance: i32, from_tag: bool) -> bool {
    let name_distance = effective_distance(cur.distance, cur.generation);
    let new_distance = effective_distance(distance, generation);
    if from_tag && cur.from_tag {
        return name_distance > new_distance;
    }
    if cur.from_tag != from_tag {
        return from_tag;
    }
    if name_distance != new_distance {
        return name_distance > new_distance;
    }
    if cur.taggerdate != taggerdate {
        return cur.taggerdate > taggerdate;
    }
    false
}

/// name-rev's `create_or_update_name()`: install/update the numeric fields when the
/// candidate beats any existing name; the caller fills `tip_name` afterward. Returns
/// whether the record was (re)claimed.
fn create_or_update_name(
    names: &mut std::collections::HashMap<ObjectId, RevName>,
    commit: &ObjectId,
    taggerdate: i64,
    generation: i32,
    distance: i32,
    from_tag: bool,
) -> bool {
    if let Some(cur) = names.get(commit) {
        if !is_better_name(cur, taggerdate, generation, distance, from_tag) {
            return false;
        }
    }
    names.insert(
        *commit,
        RevName { tip_name: String::new(), taggerdate, generation, distance, from_tag },
    );
    true
}

/// name-rev's `get_parent_name()`: the name a merge parent inherits from `name`.
fn get_parent_name(tip_name: &str, generation: i32, parent_number: usize) -> String {
    let base = tip_name.strip_suffix("^0").unwrap_or(tip_name);
    if generation > 0 {
        format!("{base}~{generation}^{parent_number}")
    } else {
        format!("{base}^{parent_number}")
    }
}

/// name-rev's `get_rev_name()` for a commit: `tip`, or `tip~<gen>` (with a trailing
/// `^0` stripped) when the commit is `gen` first-parent hops below the tip.
fn get_rev_name(names: &std::collections::HashMap<ObjectId, RevName>, commit: &ObjectId) -> Option<String> {
    let n = names.get(commit)?;
    if n.tip_name.is_empty() {
        return None;
    }
    if n.generation == 0 {
        Some(n.tip_name.clone())
    } else {
        let base = n.tip_name.strip_suffix("^0").unwrap_or(&n.tip_name);
        Some(format!("{base}~{}", n.generation))
    }
}

/// Parse (and cache) a commit's committer time and parent ids.
fn commit_info(
    cache: &mut std::collections::HashMap<ObjectId, CommitInfo>,
    repo: &gix::Repository,
    id: ObjectId,
) -> Result<CommitInfo> {
    if let Some(info) = cache.get(&id) {
        return Ok(info.clone());
    }
    let commit = repo.find_object(id)?.try_into_commit()?;
    let date = commit.time()?.seconds;
    let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
    let info = CommitInfo { date, parents };
    cache.insert(id, info.clone());
    Ok(info)
}

/// name-rev's `name_rev()`: walk `start`'s ancestry (first-parent-priority DFS via a
/// LIFO stack) propagating `tip_name` to every commit it beats, honoring the date
/// cutoff. `deref` marks a name reached by peeling a tag (adds a `^0` handle).
fn name_rev(
    names: &mut std::collections::HashMap<ObjectId, RevName>,
    cache: &mut std::collections::HashMap<ObjectId, CommitInfo>,
    repo: &gix::Repository,
    start: ObjectId,
    tip_name: &str,
    taggerdate: i64,
    from_tag: bool,
    deref: bool,
    cutoff: i64,
) -> Result<()> {
    if commit_info(cache, repo, start)?.date < cutoff {
        return Ok(());
    }
    if !create_or_update_name(names, &start, taggerdate, 0, 0, from_tag) {
        return Ok(());
    }
    names.get_mut(&start).expect("just inserted").tip_name =
        if deref { format!("{tip_name}^0") } else { tip_name.to_string() };

    // The prio_queue is used as a LIFO in git; a Vec stack reproduces it.
    let mut stack: Vec<ObjectId> = vec![start];
    while let Some(commit) = stack.pop() {
        let (cur_tip, cur_gen, cur_dist) = {
            let n = &names[&commit];
            (n.tip_name.clone(), n.generation, n.distance)
        };
        let parents = commit_info(cache, repo, commit)?.parents;
        let mut to_queue: Vec<ObjectId> = Vec::new();
        for (idx, parent) in parents.iter().enumerate() {
            let parent_number = idx + 1;
            if commit_info(cache, repo, *parent)?.date < cutoff {
                continue;
            }
            let (generation, distance) = if parent_number > 1 {
                (0, cur_dist + MERGE_TRAVERSAL_WEIGHT)
            } else {
                (cur_gen + 1, cur_dist + 1)
            };
            if create_or_update_name(names, parent, taggerdate, generation, distance, from_tag) {
                let ptip = if parent_number > 1 {
                    get_parent_name(&cur_tip, cur_gen, parent_number)
                } else {
                    cur_tip.clone()
                };
                names.get_mut(parent).expect("just inserted").tip_name = ptip;
                to_queue.push(*parent);
            }
        }
        // Push in reverse so the first parent is popped (and named) first.
        for parent in to_queue.into_iter().rev() {
            stack.push(parent);
        }
    }
    Ok(())
}

/// name-rev's `subpath_matches()`: match `filter` against `path` and each of its
/// `/`-delimited tails; returns the matched offset (`0` = full-path match).
fn subpath_matches(path: &BStr, filter: &BStr) -> Option<usize> {
    let mut offset = 0usize;
    loop {
        let sub = &path[offset..];
        if gix::glob::wildmatch(filter, sub, Mode::empty()) {
            return Some(offset);
        }
        match sub.find_byte(b'/') {
            Some(pos) => offset += pos + 1,
            None => return None,
        }
    }
}

/// The `--contains` path: `git describe --contains` == `name-rev --peel-tag
/// --name-only --no-undefined [...]` over HEAD or the given commit-ishes.
fn run_contains(
    revs: &[&str],
    all: bool,
    always: bool,
    match_pats: &[BString],
    exclude_pats: &[BString],
) -> Result<ExitCode> {
    let mut repo = gix::discover(".")?;
    repo.object_cache_size_if_unset(4 * 1024 * 1024);

    // git delegates only `refs/tags/`-prefixed patterns to name-rev, and only when
    // not `--all` (under `--all` name-rev sees every ref and no --refs/--exclude).
    let ref_filters: Vec<BString> = if all {
        Vec::new()
    } else {
        match_pats.iter().map(|p| BString::from(format!("refs/tags/{p}"))).collect()
    };
    let exclude_filters: Vec<BString> = if all {
        Vec::new()
    } else {
        exclude_pats.iter().map(|p| BString::from(format!("refs/tags/{p}"))).collect()
    };

    // Resolve the targets (HEAD by default), peeling to commits like name-rev's
    // --peel-tag on its input. A rev that won't resolve is warned about and skipped,
    // exactly as name-rev's revision setup does.
    let default = ["HEAD"];
    let inputs: &[&str] = if revs.is_empty() { &default } else { revs };
    let mut targets: Vec<ObjectId> = Vec::new();
    let mut cutoff = i64::MAX;
    let mut cache: std::collections::HashMap<ObjectId, CommitInfo> = std::collections::HashMap::new();
    for rev in inputs {
        let commit = match repo
            .rev_parse_single(*rev)
            .ok()
            .and_then(|id| id.object().ok())
            .and_then(|obj| obj.peel_to_commit().ok())
        {
            Some(c) => c,
            None => {
                eprintln!("Could not get sha1 for {rev}. Skipping.");
                continue;
            }
        };
        let oid = commit.id;
        let date = commit_info(&mut cache, &repo, oid)?.date;
        if cutoff > date {
            cutoff = date;
        }
        targets.push(oid);
    }
    if targets.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    // name-rev's slop: allow for a day of clock skew below the oldest target.
    if cutoff != i64::MAX {
        cutoff = cutoff.saturating_sub(CUTOFF_DATE_SLOP);
    }

    // Collect and rank the ref tips (`name_ref` + `cmp_by_tag_and_age`).
    let mut tips = collect_tips(&repo, all, &ref_filters, &exclude_filters, &mut cache)?;
    tips.sort_by(|a, b| {
        // Prefer tags, then older tagger date; git's QSORT is unstable on ties.
        b.from_tag.cmp(&a.from_tag).then(a.taggerdate.cmp(&b.taggerdate))
    });

    let mut names: std::collections::HashMap<ObjectId, RevName> = std::collections::HashMap::new();
    for tip in &tips {
        name_rev(
            &mut names,
            &mut cache,
            &repo,
            tip.commit,
            &tip.refname,
            tip.taggerdate,
            tip.from_tag,
            tip.deref,
            cutoff,
        )?;
    }

    // Emit each target in order (`--name-only`, `--no-undefined`): a name if found,
    // else the abbreviated hash under `--always`, else name-rev's fatal.
    for oid in &targets {
        match get_rev_name(&names, oid) {
            Some(name) => println!("{name}"),
            None => {
                if always {
                    let short = repo.find_object(*oid)?.try_into_commit()?.short_id()?;
                    println!("{short}");
                } else {
                    return fatal(format!("cannot describe '{oid}'"));
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// name-rev's `name_ref()` over every ref: apply the tags-only / ref / exclude
/// filters, peel tag chains (capturing the innermost tagger date and the `deref`
/// flag), and record the resulting commit tips.
fn collect_tips(
    repo: &gix::Repository,
    all: bool,
    ref_filters: &[BString],
    exclude_filters: &[BString],
    cache: &mut std::collections::HashMap<ObjectId, CommitInfo>,
) -> Result<Vec<Tip>> {
    let platform = repo.references()?;
    // Under `!all`, name-rev is `--tags`: only refs/tags/ (iterating tags() is the
    // same filter). Under `--all` it sees every ref.
    let iter = if all { platform.all()? } else { platform.tags()? };
    let mut tips: Vec<Tip> = Vec::new();
    for r in iter.filter_map(Result::ok) {
        let full = r.name().as_bstr().to_owned();

        // exclude filters win first.
        if exclude_filters
            .iter()
            .any(|f| subpath_matches(full.as_bstr(), f.as_bstr()).is_some())
        {
            continue;
        }
        // ref filters: at least one must match if any are present.
        if !ref_filters.is_empty()
            && !ref_filters
                .iter()
                .any(|f| subpath_matches(full.as_bstr(), f.as_bstr()).is_some())
        {
            continue;
        }

        let Some(target) = r.try_id().map(|id| id.detach()) else {
            continue;
        };
        // Peel a tag chain to the underlying commit, remembering the innermost
        // tagger date and that we dereferenced.
        let mut obj = repo.find_object(target)?;
        let mut deref = false;
        let mut taggerdate = i64::MAX;
        while obj.kind == gix::object::Kind::Tag {
            deref = true;
            taggerdate = obj
                .to_tag_ref_iter()
                .tagger()
                .ok()
                .flatten()
                .map(|s| s.seconds())
                .unwrap_or(0);
            let Ok(next) = obj.to_tag_ref_iter().target_id() else { break };
            obj = repo.find_object(next)?;
        }
        if obj.kind != gix::object::Kind::Commit {
            continue;
        }
        let commit = obj.id;
        let from_tag = full.starts_with(b"refs/tags/");
        if taggerdate == i64::MAX {
            taggerdate = commit_info(cache, repo, commit)?.date;
        }
        tips.push(Tip { commit, refname: tip_refname(full.as_bstr(), all), taggerdate, from_tag, deref });
    }
    Ok(tips)
}

/// name-rev's `add_to_tip_table()` refname shortening. Under `!all` (tags-only,
/// name-only) tags shorten to their bare name; under `--all` git strips `refs/heads/`
/// else `refs/`.
fn tip_refname(full: &BStr, all: bool) -> String {
    let short: &BStr = if !all {
        full.strip_prefix(b"refs/tags/").map(|r| r.as_bstr()).unwrap_or(full)
    } else if let Some(rest) = full.strip_prefix(b"refs/heads/") {
        rest.as_bstr()
    } else if let Some(rest) = full.strip_prefix(b"refs/") {
        rest.as_bstr()
    } else {
        full
    };
    short.to_str_lossy().into_owned()
}

/// Map a gix-shortened ref name back to git's `--all` spelling: the full name with
/// only `refs/` stripped. On a name collision git's `replace_name()` order applies —
/// annotated tag, then lightweight tag, then anything else.
fn prefixed_name(repo: &gix::Repository, short: &BStr) -> Option<BString> {
    let platform = repo.references().ok()?;
    let mut best: Option<(u8, BString)> = None;
    for r in platform.all().ok()?.filter_map(Result::ok) {
        if r.name().shorten() != short {
            continue;
        }
        let full: &[u8] = r.name().as_bstr();
        let Some(rest) = full.strip_prefix(b"refs/") else {
            continue;
        };
        let prio = if full.starts_with(b"refs/tags/") {
            // Annotated tags point at a tag object; lightweight ones at the commit.
            let annotated = r
                .try_id()
                .and_then(|id| repo.find_object(id.detach()).ok())
                .is_some_and(|obj| obj.kind == gix::object::Kind::Tag);
            if annotated {
                2
            } else {
                1
            }
        } else {
            0
        };
        let better = match &best {
            Some((best_prio, _)) => prio > *best_prio,
            None => true,
        };
        if better {
            best = Some((prio, BString::from(rest.to_vec())));
        }
    }
    best.map(|(_, name)| name)
}

/// Parse a numeric option value the way git's `--abbrev` does: C `strtol` into a
/// 32-bit `int`.
///
/// Faithful to `parse_opt_abbrev_cb` in git's `parse-options-cb.c`:
///   * optional leading whitespace, then an optional `+`/`-`, then decimal digits;
///   * `None` (git's `error: … expects a numerical value`, exit 129) is returned
///     only when no digit is consumed or non-digit bytes trail the number
///     (e.g. ``, `abc`, `0x10`, `12abc`, `8 `);
///   * overflow does not error — `strtol` saturates to `LONG_{MAX,MIN}`, so the
///     magnitude saturates to `i64::{MAX,MIN}` here;
///   * the resulting `long` is then assigned to an `int`, i.e. truncated to the
///     low 32 bits (`i64 as i32`), so `99…9` -> -1, `2^32` -> 0, `2^32+4` -> 4 —
///     matching git bit-for-bit. The caller stores this and lets `hex_len` apply
///     git's later `MINIMUM_ABBREV`/`hexsz` clamp.
fn parse_c_int(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let mut i = 0;
    // strtol skips the C `isspace` set before the sign.
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r') {
        i += 1;
    }
    let neg = matches!(b.get(i), Some(b'+' | b'-')) && {
        let n = b[i] == b'-';
        i += 1;
        n
    };
    let start = i;
    let mut acc: i64 = 0;
    let mut overflow = false;
    while let Some(&c) = b.get(i) {
        if !c.is_ascii_digit() {
            break;
        }
        let d = (c - b'0') as i64;
        match acc.checked_mul(10).and_then(|v| v.checked_add(d)) {
            Some(v) => acc = v,
            None => overflow = true,
        }
        i += 1;
    }
    // No digits, or trailing bytes strtol would not consume: git errors 129.
    if i == start || i != b.len() {
        return None;
    }
    let long_val: i64 = if overflow {
        if neg { i64::MIN } else { i64::MAX }
    } else if neg {
        -acc
    } else {
        acc
    };
    // git assigns the `long` to an `int`: keep the low 32 bits.
    Some(long_val as i32 as i64)
}

/// git's fatal convention: `fatal: <msg>` on stderr, exit 128.
fn fatal(msg: impl Display) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// git's parse-options convention for an unrecognized flag: the error, then the
/// full usage block, exit 129.
fn unknown_option_error(name: &str) -> Result<ExitCode> {
    eprintln!("error: unknown option `{name}'");
    eprint!("{USAGE}");
    Ok(ExitCode::from(129))
}

/// parse-options' message for a malformed `OPT_INTEGER`-style value (no usage block).
fn numerical_value_error(name: &str) -> Result<ExitCode> {
    eprintln!("error: option `{name}' expects a numerical value");
    Ok(ExitCode::from(129))
}

/// parse-options' message for a value-taking option given without its value.
fn missing_value_error(name: &str) -> Result<ExitCode> {
    eprintln!("error: option `{name}' requires a value");
    Ok(ExitCode::from(129))
}

/// parse-options' message for a malformed `OPT_MAGNITUDE`-style value.
fn integer_value_error(name: &str) -> Result<ExitCode> {
    eprintln!("error: option `{name}' expects an integer value with an optional k/m/g suffix");
    Ok(ExitCode::from(129))
}

const USAGE: &str = "\
usage: git describe [--all] [--tags] [--contains] [--abbrev=<n>] [<commit-ish>...]
   or: git describe [--all] [--tags] [--contains] [--abbrev=<n>] --dirty[=<mark>]
   or: git describe <blob>

    --[no-]contains       find the tag that comes after the commit
    --[no-]debug          debug search strategy on stderr
    --[no-]all            use any ref
    --[no-]tags           use any tag, even unannotated
    --[no-]long           always use long format
    --[no-]first-parent   only follow first parent
    --[no-]abbrev[=<n>]   use <n> digits to display object names
    --[no-]exact-match    only output exact matches
    --[no-]candidates <n> consider <n> most recent tags (default: 10)
    --[no-]match <pattern>
                          only consider tags matching <pattern>
    --[no-]exclude <pattern>
                          do not consider tags matching <pattern>
    --[no-]always         show abbreviated commit object as fallback
    --[no-]dirty[=<mark>] append <mark> on dirty working tree (default: \"-dirty\")
    --[no-]broken[=<mark>]
                          append <mark> on broken working tree (default: \"-broken\")
";
