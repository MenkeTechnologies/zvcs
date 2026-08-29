//! `git zrewind <duration> [--dry-run]` — restore the whole tree to a wall-clock
//! time.
//!
//! For the repo at the cwd and every nested submodule, find the HEAD the reflog
//! shows it had `<duration>` ago and `reset --hard` to it (reusing the faithful
//! porcelain reset, reflogged so the rewind is itself undoable). Refuses a dirty
//! repo — uncommitted work is never clobbered — and reports repos whose reflog
//! doesn't reach that far back. `zsnapshot` is manual named restore points; this
//! is any timestamp, no prior setup.
//!
//!   git zrewind 2h            rewind the tree to 2 hours ago
//!   git zrewind 90m --dry-run show what would move, change nothing

use crate::superset::zsince::{now_secs, parse_duration};
use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::{Command, ExitCode};

pub fn zrewind(args: &[String]) -> Result<ExitCode> {
    let mut dry = false;
    let mut spec = None;
    for a in args {
        match a.as_str() {
            "--dry-run" | "-n" => dry = true,
            s if !s.starts_with('-') => spec = Some(s.to_string()),
            _ => {}
        }
    }
    let spec = spec.ok_or_else(|| anyhow!("usage: git zrewind <duration> [--dry-run]  (e.g. 2h, 30m, 1d)"))?;
    let secs = parse_duration(&spec).ok_or_else(|| anyhow!("`{spec}` is not a duration (2h/30m/1d/90s)"))?;
    let cutoff = now_secs() - secs;

    let top = crate::setup::discover()?;
    let exe = crate::hosted::git_exe().map_err(|e| anyhow!("cannot resolve exe: {e}"))?;

    // The tree = the top repo + every initialized submodule.
    let mut repos = vec![top.clone()];
    if let Ok(Some(subs)) = top.submodules() {
        for sm in subs {
            if let Ok(Some(sub)) = sm.open() {
                repos.push(sub);
            }
        }
    }

    let (mut rewound, mut skipped) = (0, 0);
    for repo in &repos {
        let Some(workdir) = repo.workdir() else { continue };
        let name = workdir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| workdir.display().to_string());

        if repo.is_dirty().unwrap_or(false) {
            println!("skip {name}: dirty (uncommitted work — never clobbered)");
            skipped += 1;
            continue;
        }
        let Some(sha) = reflog_sha_at(repo.git_dir(), cutoff) else {
            println!("skip {name}: no history that far back");
            skipped += 1;
            continue;
        };
        let short = &sha[..sha.len().min(10)];
        if dry {
            println!("would rewind {name} → {short}");
            rewound += 1;
            continue;
        }
        let ok = Command::new(&exe)
            .args(["reset", "--hard", &sha])
            .current_dir(workdir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            println!("rewound {name} → {short}");
            rewound += 1;
        } else {
            println!("FAILED {name}: reset --hard {short}");
            skipped += 1;
        }
    }
    println!("\n{rewound} {}, {skipped} skipped (tree → {secs}s ago)", if dry { "would rewind" } else { "rewound" });
    Ok(ExitCode::SUCCESS)
}

/// The sha HEAD pointed at, at epoch `cutoff`: the NEW sha of the latest
/// `logs/HEAD` reflog entry with time ≤ cutoff. `None` if the reflog doesn't reach
/// that far back. The reflog is chronological, so once an entry is newer than the
/// cutoff every later one is too — stop there.
fn reflog_sha_at(git_dir: &Path, cutoff: i64) -> Option<String> {
    let content = std::fs::read_to_string(git_dir.join("logs/HEAD")).ok()?;
    let mut sha = None;
    for line in content.lines() {
        let Some((header, _msg)) = line.split_once('\t') else { continue };
        let toks: Vec<&str> = header.split_whitespace().collect();
        if toks.len() < 4 {
            continue;
        }
        let Ok(ts) = toks[toks.len() - 2].parse::<i64>() else { continue };
        if ts <= cutoff {
            sha = Some(toks[1].to_string());
        } else {
            break;
        }
    }
    sha
}
