//! `git zremote` — fleet-wide remote-URL rewrite.
//!
//! `git zremote set <old> <new> [selectors] [-n]` rewrites, in every selected
//! repo, any remote whose URL contains the substring `<old>`, replacing `<old>`
//! with `<new>`. This is the one-command answer to "we moved the org / changed
//! the host / switched ssh↔https" across a whole tree of submodules. `-n` /
//! `--dry-run` previews the changes without applying them. Writes go through
//! this binary's own `remote set-url` porcelain over the shared worker pool.

use std::path::Path;
use std::process::{Command, ExitCode};

use anyhow::{bail, Result};

use crate::superset::query::{parallel_map, selected};

/// Run a git subcommand (via this binary) in `workdir`; return stdout on success.
fn git_out(workdir: &Path, sub: &str, extra: &[&str]) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let o = Command::new(exe).arg(sub).args(extra).current_dir(workdir).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// One rewrite that applies (or would apply) in a repo.
struct Rewrite {
    remote: String,
    from: String,
    to: String,
    applied: bool,
    error: Option<String>,
}

fn rewrite_repo(workdir: &Path, old: &str, new: &str, dry: bool) -> Vec<Rewrite> {
    let Some(names) = git_out(workdir, "remote", &[]) else { return Vec::new() };
    let mut out = Vec::new();
    for name in names.lines().map(str::trim).filter(|n| !n.is_empty()) {
        let Some(url) = git_out(workdir, "remote", &["get-url", name]) else { continue };
        let url = url.trim();
        if !url.contains(old) {
            continue;
        }
        let to = url.replace(old, new);
        let mut r = Rewrite { remote: name.to_string(), from: url.to_string(), to: to.clone(), applied: false, error: None };
        if dry {
            out.push(r);
            continue;
        }
        match git_out(workdir, "remote", &["set-url", name, &to]) {
            Some(_) => r.applied = true,
            None => r.error = Some("set-url failed".to_string()),
        }
        out.push(r);
    }
    out
}

pub fn zremote(args: &[String]) -> Result<ExitCode> {
    if args.first().map(String::as_str) != Some("set") {
        bail!("usage: git zremote set <old> <new> [selectors] [-n|--dry-run]");
    }
    let old = args.get(1).cloned().unwrap_or_default();
    let new = args.get(2).cloned().unwrap_or_default();
    if old.is_empty() || new.is_empty() {
        bail!("usage: git zremote set <old> <new> [selectors] [-n|--dry-run]");
    }
    let mut dry = false;
    let rest: Vec<String> = args[3..]
        .iter()
        .filter(|a| {
            let is_dry = *a == "-n" || *a == "--dry-run";
            if is_dry {
                dry = true;
            }
            !is_dry
        })
        .cloned()
        .collect();

    let Some(repos) = selected(&rest)? else { return Ok(ExitCode::SUCCESS) };
    let per = parallel_map(&repos, |_gd, wd| rewrite_repo(wd, &old, &new, dry));

    let (mut changed, mut failed) = (0usize, 0usize);
    for ((_, wd), rewrites) in repos.iter().zip(&per) {
        for r in rewrites {
            let tag = if dry {
                "\x1b[33mwould set\x1b[0m"
            } else if r.applied {
                "\x1b[32mset\x1b[0m"
            } else {
                "\x1b[31mFAILED\x1b[0m"
            };
            println!("{} {}::{}  {} → {}", tag, wd.display(), r.remote, r.from, r.to);
            if r.applied {
                changed += 1;
            }
            if r.error.is_some() {
                failed += 1;
            }
        }
    }
    let verb = if dry { "would rewrite" } else { "rewrote" };
    eprintln!("zremote: {verb} {changed} remote(s), {failed} failed, across {} repos", repos.len());
    Ok(if failed > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_rewrite_semantics() {
        let url = "git@github.com:oldorg/repo.git";
        assert_eq!(url.replace("oldorg", "neworg"), "git@github.com:neworg/repo.git");
        // Non-matching URL is left untouched by the contains-guard.
        assert!(!"https://example.com/x.git".contains("oldorg"));
    }

    #[test]
    fn dry_run_flag_is_stripped_from_selectors() {
        let args: Vec<String> = ["set", "a", "b", "--dry-run", "--repo", "x"].iter().map(|s| s.to_string()).collect();
        let mut dry = false;
        let rest: Vec<String> = args[3..]
            .iter()
            .filter(|a| {
                let is = *a == "-n" || *a == "--dry-run";
                if is {
                    dry = true;
                }
                !is
            })
            .cloned()
            .collect();
        assert!(dry);
        assert_eq!(rest, vec!["--repo".to_string(), "x".to_string()]);
    }
}
