//! `git zrewind <duration> [--dry-run]` — restore the whole tree to a wall-clock
//! time.
//!
//! For the repo at the cwd and every nested submodule at any depth, find the
//! HEAD the reflog shows it had `<duration>` ago and `reset --hard` to it
//! (reusing the faithful porcelain reset, reflogged so the rewind is itself
//! undoable). Refuses a repo holding uncommitted work of its own — that is never
//! clobbered — while a submodule pointer a child moved does not count, since
//! that is the state being undone and the child is rewound in the same pass.
//! Reports repos whose reflog doesn't reach that far back. `zsnapshot` is manual named restore points; this
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

    // The tree = the top repo and every initialized submodule *at any depth*.
    // A submodule that itself has submodules is the normal shape here, and
    // stopping at the first level left the deepest repos at their current HEAD
    // while their parents moved — a tree rewound only part of the way down.
    let mut repos = vec![top.clone()];
    collect_submodules(&top, &mut repos);

    let (mut rewound, mut skipped) = (0, 0);
    for repo in &repos {
        let Some(workdir) = repo.workdir() else { continue };
        let name = workdir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| workdir.display().to_string());

        if has_local_work(&exe, workdir) {
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

/// Every initialized submodule under `repo`, at any depth, in parent-first
/// order. Mirrors `snapshot.rs`'s collect: the tree a tree-wide verb acts on is
/// the whole tree, not its first level.
fn collect_submodules(repo: &gix::Repository, out: &mut Vec<gix::Repository>) {
    let Ok(Some(subs)) = repo.submodules() else { return };
    for sm in subs {
        if let Ok(Some(sub)) = sm.open() {
            collect_submodules(&sub, out);
            out.push(sub);
        }
    }
}

/// Does this repository hold uncommitted work of its own?
///
/// Not `repo.is_dirty()`: a superproject reads as dirty the moment one of its
/// submodules moves, which is exactly the state a tree-wide rewind exists to
/// undo — so the blanket check refused every repo in a submodule tree and
/// rewound nothing. A moved gitlink is not local work; the child repo is rewound
/// in the same pass. Asked through the porcelain (`--ignore-submodules=all`
/// covers the staged gitlink as well as the worktree one) so this agrees with
/// what `git status` reports, and `-uno` because a `reset --hard` leaves
/// untracked files alone.
fn has_local_work(exe: &Path, workdir: &Path) -> bool {
    let Ok(out) = Command::new(exe)
        .args(["status", "--porcelain", "--ignore-submodules=all", "--untracked-files=no"])
        .current_dir(workdir)
        .output()
    else {
        return true; // cannot tell → refuse, never clobber
    };
    !out.stdout.is_empty()
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
