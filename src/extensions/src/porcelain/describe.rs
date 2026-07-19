use anyhow::Result;
use gix::commit::describe::SelectRef;
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
///   * `--all`                           — consider every ref (incl. branches)
///   * `--long`                          — always emit the long form
///   * `--always`                        — fall back to the abbreviated hash
///   * `--first-parent`                  — only follow the first parent
///   * `--dirty[=<mark>]`                — append `-dirty` (or `-<mark>`) if the worktree is dirty
///   * `--abbrev=<n>`                    — hex digits for the hash (`0` prints the tag only)
///   * `--candidates=<n>`                — number of candidate tags to weigh
///   * `--exact-match`                   — only print when a tag points directly at the commit
///
/// `--contains`, `--match`, `--exclude` and similar filtering options select a
/// different (name-rev / glob) algorithm that this platform does not back, and
/// are rejected explicitly rather than silently ignored.
pub fn describe(args: &[String]) -> Result<ExitCode> {
    let mut select = SelectRef::AnnotatedTags;
    let mut long = false;
    let mut always = false;
    let mut first_parent = false;
    let mut max_candidates: usize = 10;
    let mut abbrev: Option<usize> = None;
    // Outer Option: was --dirty given? Inner Option: the custom mark, if any.
    let mut dirty: Option<Option<String>> = None;
    let mut revs: Vec<&str> = Vec::new();

    for arg in args {
        let a = arg.as_str();
        match a {
            "--tags" => select = SelectRef::AllTags,
            "--all" => select = SelectRef::AllRefs,
            "--long" => long = true,
            "--always" => always = true,
            "--first-parent" => first_parent = true,
            "--exact-match" => max_candidates = 0,
            "--dirty" => dirty = Some(None),
            "--" => {} // argument separator; remaining still parsed positionally below
            _ if a.starts_with("--dirty=") => dirty = Some(Some(a["--dirty=".len()..].to_string())),
            _ if a.starts_with("--abbrev=") => {
                let v = &a["--abbrev=".len()..];
                abbrev = Some(
                    v.parse()
                        .map_err(|_| anyhow::anyhow!("invalid --abbrev value {v:?}"))?,
                );
            }
            _ if a.starts_with("--candidates=") => {
                let v = &a["--candidates=".len()..];
                max_candidates = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid --candidates value {v:?}"))?;
            }
            _ if a.starts_with("--contains") => {
                anyhow::bail!("--contains uses the name-rev algorithm, which is not ported")
            }
            _ if a.starts_with("--match") || a.starts_with("--exclude") => {
                anyhow::bail!("{a}: pattern filtering (--match/--exclude) is not ported")
            }
            _ if a.starts_with('-') => anyhow::bail!("unsupported option {a}"),
            _ => revs.push(a),
        }
    }

    if dirty.is_some() && !revs.is_empty() {
        anyhow::bail!("--dirty is incompatible with commit-ishes");
    }

    // An object cache saves ~40% of walk time and is otherwise harmless.
    let mut repo = gix::discover(".")?;
    repo.object_cache_size_if_unset(4 * 1024 * 1024);

    // Resolve the dirty mark once; the check is over tracked changes only,
    // matching git (untracked files never make describe report `-dirty`).
    let dirty_mark = match &dirty {
        Some(mark) if repo.is_dirty()? => Some(mark.clone().unwrap_or_else(|| "dirty".to_string())),
        _ => None,
    };

    // Default target is HEAD; otherwise each positional commit-ish, in order.
    if revs.is_empty() {
        let commit = repo.head_commit()?;
        describe_one(&commit, select, long, always, first_parent, max_candidates, abbrev, &dirty_mark)?;
    } else {
        for rev in &revs {
            let commit = repo.rev_parse_single(*rev)?.object()?.peel_to_commit()?;
            describe_one(&commit, select, long, always, first_parent, max_candidates, abbrev, &dirty_mark)?;
        }
    }

    // `describe_one` bails on the "no name found" case, so reaching here is success.
    Ok(ExitCode::SUCCESS)
}

/// Run the describe walk for a single already-resolved commit and print its name.
#[allow(clippy::too_many_arguments)]
fn describe_one(
    commit: &gix::Commit<'_>,
    select: SelectRef,
    long: bool,
    always: bool,
    first_parent: bool,
    max_candidates: usize,
    abbrev: Option<usize>,
    dirty_mark: &Option<String>,
) -> Result<()> {
    let platform = commit
        .describe()
        .names(select)
        .traverse_first_parent(first_parent)
        .max_candidates(max_candidates)
        .id_as_fallback(always);

    let resolution = match platform.try_resolve()? {
        Some(res) => res,
        None => {
            // No candidate name and no --always fallback: git exits non-zero here.
            let what = match select {
                SelectRef::AnnotatedTags => "annotated tag",
                SelectRef::AllTags => "tag",
                SelectRef::AllRefs => "reference",
            };
            anyhow::bail!("no {what} can describe {}", commit.id());
        }
    };

    // `--abbrev=0` is git's request to drop the `-<depth>-g<hash>` tail entirely.
    let mut out = if abbrev == Some(0) {
        match &resolution.outcome.name {
            Some(name) => name.to_string(),
            // Only reachable via --always: git still prints an abbreviated id.
            None => resolution.id.shorten()?.to_string(),
        }
    } else {
        let hex = match abbrev {
            Some(n) => n,
            None => resolution.id.shorten()?.hex_len(),
        };
        let mut format = resolution.outcome.into_format(hex);
        format.long(long);
        format.to_string()
    };

    if let Some(mark) = dirty_mark {
        out.push('-');
        out.push_str(mark);
    }

    println!("{out}");
    Ok(())
}
