use anyhow::Result;
use gix::bstr::{BStr, BString};
use gix::commit::describe::SelectRef;
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
///   * `--broken[=<mark>]`               — accepted; the mark is only appended when the
///                                         worktree diff itself fails, which cannot happen here
///
/// `--contains`, `--match` and `--exclude` need a caller-supplied candidate-name
/// map (or the name-rev algorithm); the vendored `gix` describe platform builds
/// that map internally from a `SelectRef` and exposes no hook for filtering, so
/// they are rejected explicitly rather than silently ignored.
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
    let mut max_candidates: usize = 10;
    // Raw `--abbrev` value, still signed: git clamps it late and treats 0 specially.
    let mut abbrev: Option<i64> = None;
    // Outer Option: was --dirty given? Inner Option: the custom mark, if any.
    let mut dirty: Option<Option<String>> = None;
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
            "--match" | "--exclude" => {
                if it.next().is_none() {
                    return missing_value_error(a.trim_start_matches('-'));
                }
                return pattern_unsupported(a);
            }
            _ if a.starts_with("--dirty=") => dirty = Some(Some(a["--dirty=".len()..].to_string())),
            _ if a.starts_with("--broken=") => {}
            _ if a.starts_with("--abbrev=") => match a["--abbrev=".len()..].parse::<i64>() {
                Ok(n) => abbrev = Some(n),
                Err(_) => return numerical_value_error("abbrev"),
            },
            _ if a.starts_with("--candidates=") => {
                match a["--candidates=".len()..].parse::<i64>() {
                    Ok(n) => max_candidates = n.max(0) as usize,
                    Err(_) => return integer_value_error("candidates"),
                }
            }
            _ if a.starts_with("--contains") => return contains_unsupported(),
            _ if a.starts_with("--match=") => return pattern_unsupported("--match"),
            _ if a.starts_with("--exclude=") => return pattern_unsupported("--exclude"),
            _ => return unknown_option_error(a.trim_start_matches('-')),
        }
    }

    // git rejects this combination while still parsing, before touching the repo.
    if long && abbrev == Some(0) {
        return fatal("options '--long' and '--abbrev=0' cannot be used together");
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
    if !always && !has_any_name(&repo, select)? {
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
        if let Some(code) = describe_one(&repo, &commit, &opts)? {
            return Ok(code);
        }
    } else {
        for rev in &revs {
            let commit = match repo.rev_parse_single(*rev) {
                Ok(id) => id.object()?.peel_to_commit()?,
                Err(_) => return fatal(format!("Not a valid object name {rev}")),
            };
            if let Some(code) = describe_one(&repo, &commit, &opts)? {
                return Ok(code);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
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

/// Run the describe walk for a single already-resolved commit and print its name.
///
/// `Ok(None)` means the name was printed; `Ok(Some(code))` is git's fatal exit for
/// this commit, which stops the caller from describing any later commit-ish.
fn describe_one(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    opts: &Options,
) -> Result<Option<ExitCode>> {
    let exact_only = opts.max_candidates == 0;
    let platform = commit
        .describe()
        .names(opts.select)
        .traverse_first_parent(opts.first_parent)
        .max_candidates(opts.max_candidates)
        // With --exact-match git dies rather than falling back to the hash, so the
        // fallback is withheld here and applied by hand below.
        .id_as_fallback(opts.always && !exact_only);

    let mut resolution = match platform.try_resolve()? {
        Some(res) => res,
        None => {
            let id = commit.id();
            if exact_only {
                return fatal(format!("no tag exactly matches '{id}'")).map(Some);
            }
            // Distinguish "there are lightweight tags you are ignoring" from "there
            // is nothing reachable at all" the way git does: re-run the same walk
            // with unannotated tags admitted and see whether that finds a name.
            let unannotated_would_help = opts.select == SelectRef::AnnotatedTags
                && commit
                    .describe()
                    .names(SelectRef::AllTags)
                    .traverse_first_parent(opts.first_parent)
                    .max_candidates(opts.max_candidates)
                    .try_resolve()?
                    .is_some();
            return if unannotated_would_help {
                fatal(format!(
                    "No annotated tags can describe '{id}'.\nHowever, there were unannotated tags: try --tags."
                ))
            } else {
                fatal(format!(
                    "No tags can describe '{id}'.\nTry --always, or create some tags."
                ))
            }
            .map(Some);
        }
    };

    // gix shortens ref names (`refs/heads/main` -> `main`); `--all` wants git's
    // form, which only strips `refs/`. Rewrite before formatting so the long form
    // (`heads/main-2-gabc1234`) picks the prefixed name up as well.
    if opts.prefix_names {
        let prefixed = resolution
            .outcome
            .name
            .as_deref()
            .and_then(|name| prefixed_name(repo, name));
        if let Some(full) = prefixed {
            resolution.outcome.name = Some(Cow::Owned(full));
        }
    }

    // `--abbrev=0` is git's request to drop the `-<depth>-g<hash>` tail entirely.
    let mut out = if opts.abbrev == Some(0) {
        match &resolution.outcome.name {
            Some(name) => name.to_string(),
            // Only reachable via --always, where git prints the id in full.
            None => resolution.id.to_string(),
        }
    } else {
        let hex = hex_len(&resolution, opts.abbrev)?;
        let mut format = resolution.outcome.into_format(hex);
        format.long(opts.long);
        format.to_string()
    };

    if let Some(mark) = &opts.dirty_mark {
        out.push('-');
        out.push_str(mark);
    }

    println!("{out}");
    Ok(None)
}

/// git's abbreviation clamp: below `MINIMUM_ABBREV` it widens to 4, above the hash
/// width it saturates, and an absent value means "let the repo size decide".
fn hex_len(resolution: &gix::commit::describe::Resolution<'_>, abbrev: Option<i64>) -> Result<usize> {
    let full = resolution.id.kind().len_in_hex();
    Ok(match abbrev {
        Some(n) => (n.max(4) as usize).min(full),
        None => resolution.id.shorten()?.hex_len(),
    })
}

/// Does any ref the selector would collect exist at all?
///
/// This is git's `names.nr` test. Under the default selector git still collects
/// unannotated tags into `names` (they just lose the candidate contest later), so
/// a repo with only lightweight tags is *not* "no names found".
fn has_any_name(repo: &gix::Repository, select: SelectRef) -> Result<bool> {
    let platform = repo.references()?;
    let mut iter = match select {
        SelectRef::AllRefs => platform.all()?,
        SelectRef::AllTags | SelectRef::AnnotatedTags => platform.tags()?,
    };
    Ok(iter.next().is_some())
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

fn pattern_unsupported(flag: &str) -> Result<ExitCode> {
    eprintln!(
        "fatal: {flag}: pattern filtering is not yet ported — the vendored describe platform \
         builds its candidate names internally and cannot be filtered"
    );
    Ok(ExitCode::from(128))
}

fn contains_unsupported() -> Result<ExitCode> {
    eprintln!("fatal: --contains is not yet ported — it needs the name-rev algorithm");
    Ok(ExitCode::from(128))
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
