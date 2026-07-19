use anyhow::{anyhow, bail, Result};
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// `git rev-list` — list commit ids reachable from the given revisions, in
/// reverse-chronological (commit-date, newest-first) order, matching stock git.
///
/// Supported invocation forms (the ones the meta workflow leans on):
///   * `rev-list <rev>...`            — commits reachable from each `<rev>`
///   * `rev-list ^<rev>`              — exclude commits reachable from `<rev>`
///   * `rev-list <a>..<b>`            — reachable from `<b>` but not `<a>` (empty
///                                       side defaults to `HEAD`)
///   * `rev-list --all`              — tips are every ref under `refs/`
///   * `--count`                     — print only the number of commits
///   * `-n <n>` / `-n<n>` / `--max-count=<n>` — limit the number listed
///   * `--reverse`                   — reverse the output (limit applied first)
///   * `--first-parent`             — follow only the first parent of merges
///
/// Genuinely unsupported forms are rejected with a precise message rather than
/// producing wrong output: symmetric-difference ranges (`<a>...<b>`), pathspec
/// filtering (`-- <path>`), and any other flag.
pub fn rev_list(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    let mut count_only = false;
    let mut reverse = false;
    let mut first_parent = false;
    let mut use_all = false;
    let mut max_count: Option<usize> = None;
    let mut tips: Vec<ObjectId> = Vec::new();
    let mut hidden: Vec<ObjectId> = Vec::new();

    // Resolve a revision to the id of the commit it points at (peeling tags).
    let resolve = |spec: &str| -> Result<ObjectId> {
        let commit = repo
            .rev_parse_single(spec)?
            .object()?
            .peel_to_commit()
            .map_err(|e| anyhow!("{spec}: not a commit: {e}"))?;
        Ok(commit.id)
    };

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--count" => count_only = true,
            "--reverse" => reverse = true,
            "--first-parent" => first_parent = true,
            "--all" => use_all = true,
            "-n" => {
                i += 1;
                let n = args
                    .get(i)
                    .ok_or_else(|| anyhow!("option `-n` requires a value"))?;
                max_count = Some(
                    n.parse::<usize>()
                        .map_err(|_| anyhow!("invalid count `{n}` for `-n`"))?,
                );
            }
            "--" => {
                if i + 1 < args.len() {
                    bail!("pathspec filtering is not supported");
                }
            }
            s if s.starts_with("--max-count=") => {
                let n = &s["--max-count=".len()..];
                max_count = Some(
                    n.parse::<usize>()
                        .map_err(|_| anyhow!("invalid count `{n}` for `--max-count`"))?,
                );
            }
            s if s.starts_with("-n") => {
                let n = &s[2..];
                max_count = Some(
                    n.parse::<usize>()
                        .map_err(|_| anyhow!("invalid count `{n}` for `-n`"))?,
                );
            }
            s if s.starts_with('^') => {
                hidden.push(resolve(&s[1..])?);
            }
            s if s.contains("...") => {
                bail!("symmetric-difference range `{s}` is not supported");
            }
            s if s.contains("..") => {
                let (left, right) = s
                    .split_once("..")
                    .ok_or_else(|| anyhow!("invalid range `{s}`"))?;
                let left = if left.is_empty() { "HEAD" } else { left };
                let right = if right.is_empty() { "HEAD" } else { right };
                hidden.push(resolve(left)?);
                tips.push(resolve(right)?);
            }
            s if s.starts_with('-') => {
                bail!("unsupported option `{s}`");
            }
            s => {
                tips.push(resolve(s)?);
            }
        }
        i += 1;
    }

    // `--all`: seed the tips with the commit each ref under `refs/` resolves to.
    // Refs that don't peel to a commit (e.g. a tag to a blob) are skipped, as git does.
    if use_all {
        for reference in repo.references()?.all()? {
            let reference = reference.map_err(|e| anyhow!("{e}"))?;
            let Ok(id) = reference.into_fully_peeled_id() else {
                continue;
            };
            let Ok(object) = id.object() else { continue };
            if let Ok(commit) = object.peel_to_commit() {
                tips.push(commit.id);
            }
        }
    }

    if tips.is_empty() {
        bail!("no starting revision given");
    }

    let mut platform = repo
        .rev_walk(tips)
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
    if first_parent {
        platform = platform.first_parent_only();
    }
    if !hidden.is_empty() {
        platform = platform.with_hidden(hidden);
    }

    let mut out: Vec<ObjectId> = Vec::new();
    for info in platform.all()? {
        if let Some(max) = max_count {
            if out.len() >= max {
                break;
            }
        }
        out.push(info?.id);
    }

    if reverse {
        out.reverse();
    }

    if count_only {
        println!("{}", out.len());
    } else {
        for id in out {
            println!("{id}");
        }
    }

    Ok(ExitCode::SUCCESS)
}
