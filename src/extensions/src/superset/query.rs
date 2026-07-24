//! Parallel query and action verbs over the whole indexed repo set — the
//! many-repo half of a massively-parallel VCS.
//!
//! Each verb selects repos via [`Selector`] (so every one honors the same
//! `--repo`/`--dirty`/`--ahead`/`--behind`/`--claimed`/`--session` filters as
//! `zforeach`), probes or acts on them concurrently through [`parallel_map`] (a
//! bounded worker pool over the machine's cores), and prints an aggregated,
//! selection-ordered view. Queries are native gix/filesystem reads — no fork, no
//! dependency on a ported porcelain — so they are fast and reliable across
//! hundreds of repos. The one action, `zpull`, reuses the same native fetch +
//! fast-forward `zsync` performs.

use anyhow::Result;
use gix::bstr::ByteSlice;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;

use crate::superset::select::Selector;

/// Map `f` over the selected `(git_dir, workdir)` repos in parallel, returning
/// results in the same order. A bounded pool (machine cores, capped at 16) work-
/// steals via one atomic counter, exactly as `zforeach` does.
pub(crate) fn parallel_map<T, F>(repos: &[(PathBuf, PathBuf)], f: F) -> Vec<T>
where
    F: Fn(&Path, &Path) -> T + Sync,
    T: Send,
{
    let n = repos.len();
    let slots: Vec<Mutex<Option<T>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let workers = thread::available_parallelism().map(|c| c.get().min(16)).unwrap_or(4).min(n.max(1));
    thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= n {
                    break;
                }
                let (git_dir, workdir) = &repos[i];
                let v = f(git_dir, workdir);
                *slots[i].lock().unwrap() = Some(v);
            });
        }
    });
    slots.into_iter().map(|m| m.into_inner().unwrap().expect("every slot filled")).collect()
}

/// Select repos and print a "no repos matched" note when empty. Returns the
/// selected repos, or `None` when there is nothing to do.
pub(crate) fn selected(args: &[String]) -> Result<Option<Vec<(PathBuf, PathBuf)>>> {
    select_repos(&Selector::parse(args).0)
}

/// Resolve a parsed selector to its repos, printing "no repos matched" and
/// returning `None` when empty. The shared tail of every selector verb — those
/// with no positional call [`selected`], those with a positional peel it off
/// `Selector::parse`'s leftovers and then call this.
pub(crate) fn select_repos(sel: &Selector) -> Result<Option<Vec<(PathBuf, PathBuf)>>> {
    let repos = sel.select()?;
    if repos.is_empty() {
        println!("no repos matched");
        return Ok(None);
    }
    Ok(Some(repos))
}

/// Open a repo by its git dir, mapping the error to a short display string.
pub(crate) fn probe<T>(git_dir: &Path, f: impl FnOnce(&gix::Repository) -> T, on_err: impl FnOnce(String) -> T) -> T {
    match gix::open(git_dir) {
        Ok(repo) => f(&repo),
        Err(e) => on_err(format!("(open failed: {e})")),
    }
}

/// `git zheads [selectors]` — each repo's checked-out branch (or detached HEAD),
/// short HEAD id, and a `*` when the worktree is dirty.
pub fn zheads(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    let lines = parallel_map(&repos, |gd, _| probe(gd, head_line, |e| e));
    let width = repos.iter().map(|(_, w)| w.display().to_string().len()).max().unwrap_or(0);
    for ((_, wd), line) in repos.iter().zip(&lines) {
        println!("{:<width$}  {line}", wd.display().to_string());
    }
    Ok(ExitCode::SUCCESS)
}

/// One repo's HEAD summary: dirty marker, branch or `(detached)`/`(unborn)`, id.
fn head_line(repo: &gix::Repository) -> String {
    let dirty = if repo.is_dirty().unwrap_or(false) { "*" } else { " " };
    let sha = repo
        .head()
        .ok()
        .and_then(|mut h| h.try_peel_to_id().ok().flatten())
        .map(|id| id.to_hex_with_len(8).to_string());
    match (repo.head_name().ok().flatten(), sha) {
        (Some(name), Some(s)) => format!("{dirty} {} {s}", name.shorten()),
        (None, Some(s)) => format!("{dirty} (detached) {s}"),
        _ => format!("{dirty} (unborn)"),
    }
}

/// `git zdirty [selectors]` — only the repos with uncommitted changes.
pub fn zdirty(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    let dirty = parallel_map(&repos, |gd, _| probe(gd, |r| r.is_dirty().unwrap_or(false), |_| false));
    let mut shown = 0usize;
    for ((_, wd), d) in repos.iter().zip(&dirty) {
        if *d {
            println!("{}", wd.display());
            shown += 1;
        }
    }
    eprintln!("zdirty: {shown} dirty of {} indexed", repos.len());
    Ok(ExitCode::SUCCESS)
}

/// `git zbranches [selectors]` — each repo's local branch names.
pub fn zbranches(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    let lists = parallel_map(&repos, |gd, _| probe(gd, ref_names_branches, |e| e));
    for ((_, wd), names) in repos.iter().zip(&lists) {
        println!("== {} ==\n{names}", wd.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn ref_names_branches(repo: &gix::Repository) -> String {
    let Ok(platform) = repo.references() else { return "(no refs)".into() };
    let Ok(iter) = platform.local_branches() else { return "(no refs)".into() };
    let mut names: Vec<String> = iter.flatten().map(|r| r.name().shorten().to_string()).collect();
    names.sort();
    if names.is_empty() {
        "(none)".into()
    } else {
        names.join("  ")
    }
}

/// `git ztags [selectors]` — each repo's tag count.
pub fn ztags(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    let counts = parallel_map(&repos, |gd, _| probe(gd, tag_count, |_| 0usize));
    let width = repos.iter().map(|(_, w)| w.display().to_string().len()).max().unwrap_or(0);
    for ((_, wd), n) in repos.iter().zip(&counts) {
        println!("{:<width$}  {n} tag(s)", wd.display().to_string());
    }
    Ok(ExitCode::SUCCESS)
}

fn tag_count(repo: &gix::Repository) -> usize {
    let Ok(platform) = repo.references() else { return 0 };
    platform.tags().map(|it| it.flatten().count()).unwrap_or(0)
}

/// `git zremotes [selectors]` — each repo's remotes and their fetch URLs.
pub fn zremotes(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    let lists = parallel_map(&repos, |gd, _| probe(gd, remote_lines, |e| e));
    for ((_, wd), remotes) in repos.iter().zip(&lists) {
        println!("== {} ==\n{remotes}", wd.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn remote_lines(repo: &gix::Repository) -> String {
    let mut out = Vec::new();
    for name in repo.remote_names() {
        let url = repo
            .find_remote(&*name)
            .ok()
            .and_then(|r| r.url(gix::remote::Direction::Fetch).map(|u| u.to_bstring().to_string()))
            .unwrap_or_default();
        out.push(format!("  {name}\t{url}"));
    }
    if out.is_empty() {
        "  (none)".into()
    } else {
        out.join("\n")
    }
}

/// `git zsize [selectors]` — each repo's on-disk `.git` size, largest first.
pub fn zsize(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    let sizes = parallel_map(&repos, |gd, _| dir_size(gd));
    let mut rows: Vec<(u64, String)> = repos
        .iter()
        .zip(&sizes)
        .map(|((_, wd), s)| (*s, wd.display().to_string()))
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    let total: u64 = sizes.iter().sum();
    let width = rows.iter().map(|(_, p)| p.len()).max().unwrap_or(0);
    for (size, path) in &rows {
        println!("{:<width$}  {:>8}", path, crate::superset::gitls::human_size(*size));
    }
    eprintln!("zsize: {} total across {} repos", crate::superset::gitls::human_size(total), rows.len());
    Ok(ExitCode::SUCCESS)
}

/// Recursively sum regular-file sizes under `p`, not following symlinks.
pub(crate) fn dir_size(p: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(p) else { return 0 };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            total += dir_size(&entry.path());
        } else if let Ok(m) = entry.metadata() {
            total += m.len();
        }
    }
    total
}

/// `git zage [selectors]` — how long ago each repo's HEAD commit was made.
pub fn zage(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    let now = crate::date::now_seconds();
    let ages = parallel_map(&repos, |gd, _| probe(gd, |r| head_age(r, now), |e| e));
    let width = repos.iter().map(|(_, w)| w.display().to_string().len()).max().unwrap_or(0);
    for ((_, wd), age) in repos.iter().zip(&ages) {
        println!("{:<width$}  {age}", wd.display().to_string());
    }
    Ok(ExitCode::SUCCESS)
}

fn head_age(repo: &gix::Repository, now: i64) -> String {
    match repo.head_commit().ok().and_then(|c| c.time().ok()) {
        Some(t) => crate::date::show_date_relative(t.seconds, now),
        None => "(unborn)".into(),
    }
}

/// `git zpull [selectors]` — fetch and fast-forward every selected repo in
/// parallel, using the same native, ff-only reconcile as `zsync` (a dirty or
/// diverged repo is reported, never forced).
pub fn zpull(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    let results = parallel_map(&repos, |gd, _| match gix::open(gd) {
        Ok(repo) => crate::superset::reconcile_repo(&repo).map_err(|e| e.to_string()),
        Err(e) => Err(format!("open failed: {e}")),
    });
    let (mut ok, mut failed) = (0usize, 0usize);
    for ((git_dir, wd), res) in repos.iter().zip(&results) {
        match res {
            Ok(summary) => {
                println!("{}: {summary}", wd.display());
                ok += 1;
            }
            Err(e) => {
                eprintln!("{}: {e}", wd.display());
                let _ = crate::db::record_failure(git_dir, "zpull", &format!("{}: {e}", wd.display()));
                failed += 1;
            }
        }
    }
    eprintln!("zpull: {ok} ok, {failed} failed ({} repos)", repos.len());
    Ok(if failed > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

/// HEAD commit time (unix seconds) of a repo, or `None` when unborn.
fn head_seconds(repo: &gix::Repository) -> Option<i64> {
    repo.head_commit().ok().and_then(|c| c.time().ok()).map(|t| t.seconds)
}

/// `git zstale [selectors] [<days>]` — indexed repos whose HEAD commit is older
/// than `<days>` (default 90), with how long ago \(em find abandoned repos.
pub fn zstale(args: &[String]) -> Result<ExitCode> {
    let (sel, rest) = Selector::parse(args);
    let days: i64 = rest.iter().find_map(|a| a.parse().ok()).unwrap_or(90);
    let Some(repos) = select_repos(&sel)? else { return Ok(ExitCode::SUCCESS) };
    let now = crate::date::now_seconds();
    let cutoff = now - days * 86_400;
    let times = parallel_map(&repos, |gd, _| probe(gd, head_seconds, |_| None));
    let width = repos.iter().map(|(_, w)| w.display().to_string().len()).max().unwrap_or(0);
    let mut shown = 0usize;
    for ((_, wd), t) in repos.iter().zip(&times) {
        if let Some(secs) = t {
            if *secs < cutoff {
                println!("{:<width$}  {}", wd.display().to_string(), crate::date::show_date_relative(*secs, now));
                shown += 1;
            }
        }
    }
    eprintln!("zstale: {shown} of {} indexed older than {days} day(s)", repos.len());
    Ok(ExitCode::SUCCESS)
}

/// `git zlast [selectors]` — indexed repos ordered by HEAD commit time, most
/// recently committed first.
pub fn zlast(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    let now = crate::date::now_seconds();
    let times = parallel_map(&repos, |gd, _| probe(gd, head_seconds, |_| None));
    let mut rows: Vec<(i64, String)> = repos
        .iter()
        .zip(&times)
        .filter_map(|((_, wd), t)| t.map(|s| (s, wd.display().to_string())))
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    let width = rows.iter().map(|(_, p)| p.len()).max().unwrap_or(0);
    for (secs, path) in &rows {
        println!("{path:<width$}  {}", crate::date::show_date_relative(*secs, now));
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zfiles [selectors]` — tracked file count of each indexed repo, largest
/// first.
pub fn zfiles(args: &[String]) -> Result<ExitCode> {
    let Some(repos) = selected(args)? else { return Ok(ExitCode::SUCCESS) };
    let counts = parallel_map(&repos, |gd, _| probe(gd, tracked_count, |_| 0usize));
    let mut rows: Vec<(usize, String)> = repos
        .iter()
        .zip(&counts)
        .map(|((_, wd), c)| (*c, wd.display().to_string()))
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let width = rows.iter().map(|(_, p)| p.len()).max().unwrap_or(0);
    for (c, p) in &rows {
        println!("{p:<width$}  {c} file(s)");
    }
    Ok(ExitCode::SUCCESS)
}

fn tracked_count(repo: &gix::Repository) -> usize {
    repo.open_index().map(|i| i.entries().len()).unwrap_or(0)
}

/// `git zbig [selectors] [<n>]` — the largest tracked files across every indexed
/// repo, top `<n>` (default 20).
pub fn zbig(args: &[String]) -> Result<ExitCode> {
    let (sel, rest) = Selector::parse(args);
    let n: usize = rest.iter().find_map(|a| a.parse().ok()).unwrap_or(20);
    let Some(repos) = select_repos(&sel)? else { return Ok(ExitCode::SUCCESS) };
    let per = parallel_map(&repos, |gd, wd| big_files(gd, wd));
    let mut all: Vec<(u64, String)> = per.into_iter().flatten().collect();
    all.sort_by(|a, b| b.0.cmp(&a.0));
    all.truncate(n);
    for (size, path) in &all {
        println!("{:>9}  {path}", crate::superset::gitls::human_size(*size));
    }
    eprintln!("zbig: top {} tracked files across {} repos", all.len(), repos.len());
    Ok(ExitCode::SUCCESS)
}

/// A repo's tracked regular files as `(size, full-path)`, capped to its own top
/// 200 so one huge repo cannot dominate memory before the global merge.
fn big_files(git_dir: &Path, workdir: &Path) -> Vec<(u64, String)> {
    let Ok(repo) = gix::open(git_dir) else { return Vec::new() };
    let Ok(index) = repo.open_index() else { return Vec::new() };
    let mut v: Vec<(u64, String)> = Vec::new();
    for entry in index.entries() {
        if entry.stage() != gix::index::entry::Stage::Unconflicted {
            continue;
        }
        let full = workdir.join(entry.path(&index).to_path_lossy());
        if let Ok(m) = std::fs::symlink_metadata(&full) {
            if m.is_file() {
                v.push((m.len(), full.display().to_string()));
            }
        }
    }
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.truncate(200);
    v
}
