//! Cross-repo search and analytics over the indexed set — `zgrep`, `zahead`,
//! `zbehind`, `zauthors`, `zhot`, `zconflicts`.
//!
//! Each verb fans a native gix/regex probe across every indexed repo through the
//! shared [`parallel_map`] worker pool and aggregates the result, so a whole tree
//! of repos can be searched or profiled in one command. All honor the same
//! [`Selector`] filters as `zforeach`.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{bail, Result};
use gix::bstr::ByteSlice;

use crate::superset::query::{parallel_map, probe, selected};
use crate::superset::select::Selector;

/// `git zgrep [selectors] [-i] <pattern>` — search the tracked file content of
/// every indexed repo for `<pattern>` (a regular expression), in parallel, and
/// print `path:line:text` for each match. `-i` is case-insensitive.
pub fn zgrep(args: &[String]) -> Result<ExitCode> {
    let (sel, rest) = Selector::parse(args);
    let mut icase = false;
    let mut pattern: Option<&str> = None;
    for a in &rest {
        if a == "-i" {
            icase = true;
        } else if pattern.is_none() {
            pattern = Some(a);
        }
    }
    let Some(pattern) = pattern else {
        bail!("usage: git zgrep [selectors] [-i] <pattern>");
    };
    let re = regex::bytes::RegexBuilder::new(pattern)
        .case_insensitive(icase)
        .build()
        .map_err(|e| anyhow::anyhow!("bad pattern: {e}"))?;

    let repos = sel.select()?;
    if repos.is_empty() {
        println!("no repos matched");
        return Ok(ExitCode::SUCCESS);
    }
    let re = &re;
    let results = parallel_map(&repos, |gd, wd| grep_repo(gd, wd, re));
    let mut total = 0usize;
    for lines in &results {
        for line in lines {
            println!("{line}");
            total += 1;
        }
    }
    eprintln!("zgrep: {total} match(es) across {} repos", repos.len());
    Ok(ExitCode::SUCCESS)
}

/// Grep one repo's tracked (non-conflicted) files in the worktree.
fn grep_repo(git_dir: &Path, workdir: &Path, re: &regex::bytes::Regex) -> Vec<String> {
    let Ok(repo) = gix::open(git_dir) else { return Vec::new() };
    let Ok(index) = repo.open_index() else { return Vec::new() };
    let mut out = Vec::new();
    for entry in index.entries() {
        if entry.stage() != gix::index::entry::Stage::Unconflicted {
            continue;
        }
        let rel = entry.path(&index);
        let full = workdir.join(rel.to_path_lossy());
        let Ok(bytes) = std::fs::read(&full) else { continue };
        if bytes.contains(&0) {
            continue; // binary — skip, like git grep
        }
        for (n, line) in bytes.split(|&b| b == b'\n').enumerate() {
            if re.is_match(line) {
                out.push(format!("{}:{}:{}", full.display(), n + 1, String::from_utf8_lossy(line)));
            }
        }
    }
    out
}

/// `git zahead [selectors]` — indexed repos with commits not yet on their
/// upstream, and how many.
pub fn zahead(args: &[String]) -> Result<ExitCode> {
    ahead_behind_verb(args, true)
}

/// `git zbehind [selectors]` — indexed repos whose upstream has commits they lack.
pub fn zbehind(args: &[String]) -> Result<ExitCode> {
    ahead_behind_verb(args, false)
}

fn ahead_behind_verb(args: &[String], want_ahead: bool) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    let counts = parallel_map(&repos, |gd, _| probe(gd, ahead_behind, |_| None));
    let label = if want_ahead { "ahead" } else { "behind" };
    let mut shown = 0usize;
    for ((_, wd), ab) in repos.iter().zip(&counts) {
        if let Some((ahead, behind)) = ab {
            let n = if want_ahead { *ahead } else { *behind };
            if n > 0 {
                println!("{}  {n} {label}", wd.display());
                shown += 1;
            }
        }
    }
    eprintln!("z{label}: {shown} of {} indexed", repos.len());
    Ok(ExitCode::SUCCESS)
}

/// `(ahead, behind)` commit counts of HEAD vs its configured upstream, or `None`
/// when there is no upstream. Mirrors `porcelain::status`'s tracking logic.
pub(crate) fn ahead_behind(repo: &gix::Repository) -> Option<(usize, usize)> {
    let branch_ref = repo.head_ref().ok().flatten()?;
    let Some(Ok(upstream_name)) = branch_ref.remote_tracking_ref_name(gix::remote::Direction::Fetch)
    else {
        return None;
    };
    let upstream_full = upstream_name.as_bstr().to_str_lossy().into_owned();
    let upstream_ref = repo.try_find_reference(upstream_full.as_str()).ok()??;
    let upstream_id = upstream_ref.into_fully_peeled_id().ok()?.detach();
    let local_id = repo.head_id().ok()?.detach();
    Some((count_commits(repo, local_id, upstream_id), count_commits(repo, upstream_id, local_id)))
}

/// Commits reachable from `tip` but not `hidden`.
fn count_commits(repo: &gix::Repository, tip: gix::ObjectId, hidden: gix::ObjectId) -> usize {
    repo.rev_walk(Some(tip))
        .with_hidden(Some(hidden))
        .all()
        .map(|w| w.take_while(Result::is_ok).count())
        .unwrap_or(0)
}

/// `git zauthors [selectors]` — commit counts by author across every indexed
/// repo's history, aggregated and ranked.
pub fn zauthors(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    let per = parallel_map(&repos, |gd, _| probe(gd, author_counts, |_| HashMap::new()));
    let mut total: HashMap<String, usize> = HashMap::new();
    for m in per {
        for (who, n) in m {
            *total.entry(who).or_default() += n;
        }
    }
    let mut rows: Vec<(usize, String)> = total.into_iter().map(|(k, v)| (v, k)).collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    for (n, who) in &rows {
        println!("{n:>7}  {who}");
    }
    eprintln!("zauthors: {} distinct across {} repos", rows.len(), repos.len());
    Ok(ExitCode::SUCCESS)
}

/// Commit counts by `Name <email>` over a repo's full HEAD history.
fn author_counts(repo: &gix::Repository) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    let Ok(head) = repo.head_id() else { return m };
    let Ok(walk) = repo.rev_walk(Some(head.detach())).all() else { return m };
    for info in walk.flatten() {
        if let Ok(commit) = repo.find_commit(info.id) {
            if let Ok(sig) = commit.author() {
                *m.entry(format!("{} <{}>", sig.name, sig.email)).or_default() += 1;
            }
        }
    }
    m
}

/// `git zhot [selectors] [<days>]` — indexed repos ranked by commits made in the
/// last `<days>` (default 30), most active first.
pub fn zhot(args: &[String]) -> Result<ExitCode> {
    let (sel, rest) = Selector::parse(args);
    let days: i64 = rest.iter().find_map(|a| a.parse().ok()).unwrap_or(30);
    let repos = sel.select()?;
    if repos.is_empty() {
        println!("no repos matched");
        return Ok(ExitCode::SUCCESS);
    }
    let cutoff = crate::date::now_seconds() - days * 86_400;
    let counts = parallel_map(&repos, |gd, _| probe(gd, |r| recent_commits(r, cutoff), |_| 0usize));
    let mut rows: Vec<(usize, String)> = repos
        .iter()
        .zip(&counts)
        .map(|((_, wd), c)| (*c, wd.display().to_string()))
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let width = rows.iter().map(|(_, p)| p.len()).max().unwrap_or(0);
    for (c, p) in &rows {
        println!("{p:<width$}  {c} commit(s)");
    }
    eprintln!("zhot: commits in the last {days} day(s)");
    Ok(ExitCode::SUCCESS)
}

/// Commits in a repo whose commit time is at or after `cutoff`. The walk is
/// bounded (merge history is not strictly time-ordered, so it cannot early-exit)
/// to keep a pathologically deep repo from dominating the fan-out.
fn recent_commits(repo: &gix::Repository, cutoff: i64) -> usize {
    let Ok(head) = repo.head_id() else { return 0 };
    let Ok(walk) = repo.rev_walk(Some(head.detach())).all() else { return 0 };
    let mut count = 0usize;
    for (examined, info) in walk.flatten().enumerate() {
        if examined >= 20_000 {
            break;
        }
        if let Ok(commit) = repo.find_commit(info.id) {
            if commit.time().map(|t| t.seconds >= cutoff).unwrap_or(false) {
                count += 1;
            }
        }
    }
    count
}

/// `git zconflicts [selectors]` — indexed repos in the middle of a merge, rebase,
/// cherry-pick, revert, or bisect, or with unmerged (conflicted) index entries.
pub fn zconflicts(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    let states = parallel_map(&repos, |gd, _| conflict_state(gd));
    let mut shown = 0usize;
    for ((_, wd), state) in repos.iter().zip(&states) {
        if let Some(s) = state {
            println!("{}  [{s}]", wd.display());
            shown += 1;
        }
    }
    eprintln!("zconflicts: {shown} of {} indexed mid-operation or conflicted", repos.len());
    Ok(ExitCode::SUCCESS)
}

/// A repo's in-progress operation(s) / conflict state, or `None` when clean.
fn conflict_state(git_dir: &Path) -> Option<String> {
    let mut ops: Vec<&str> = Vec::new();
    let has = |n: &str| git_dir.join(n).exists();
    if has("MERGE_HEAD") {
        ops.push("merge");
    }
    if has("rebase-merge") || has("rebase-apply") {
        ops.push("rebase");
    }
    if has("CHERRY_PICK_HEAD") {
        ops.push("cherry-pick");
    }
    if has("REVERT_HEAD") {
        ops.push("revert");
    }
    if has("BISECT_LOG") {
        ops.push("bisect");
    }
    let unmerged = gix::open(git_dir)
        .ok()
        .and_then(|r| r.open_index().ok())
        .map(|idx| idx.entries().iter().any(|e| e.stage() != gix::index::entry::Stage::Unconflicted))
        .unwrap_or(false);
    if unmerged {
        ops.push("conflicts");
    }
    (!ops.is_empty()).then(|| ops.join(", "))
}
