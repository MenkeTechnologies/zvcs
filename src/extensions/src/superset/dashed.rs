//! `git zdashed [<dir>]` — install a `git-<verb>` symlink for every builtin and
//! superset verb into `<dir>` (default `$ZVCS_HOME/bin`, i.e. `~/.zvcs/bin`), so
//! the dashed external form works when zvcs shadows `git`: `git-status`,
//! `git-commit`, `git-for-each-ref`, … all resolve to this binary, which strips
//! the `git-` prefix from argv[0] and dispatches the verb. Needed once stock git
//! is uninstalled and nothing else on PATH provides those dashed forms.
//!
//! The verb set is read from the dispatch tables ([`PORCELAIN_VERBS`] +
//! [`SUPERSET_VERBS`]), never hardcoded, so it can't drift as verbs are added.
//! Idempotent: a correct symlink is left alone, a stale one is repointed, and a
//! real (non-symlink) file of the same name is never clobbered.
//!
//! The link loop lives in [`install_links`] because [`crate::superset::zshadow`]
//! installs the same set as one step of the full shadow install.

use crate::dispatch::{PORCELAIN_VERBS, SUPERSET_VERBS};
use anyhow::{Context, Result};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// What a symlink install did: links created, links already pointing at the
/// target, and names left untouched because a real file holds them.
#[derive(Default)]
pub struct LinkStats {
    pub created: usize,
    pub current: usize,
    pub skipped: usize,
}

/// The path the `git-<verb>` links should point at: the sibling `git` when it
/// already exists (a relative link, so the dashed forms track whatever the shim
/// points at and survive rebuilds), else this binary by absolute path.
pub fn link_target(dir: &Path) -> Result<PathBuf> {
    if dir.join("git").exists() {
        Ok(PathBuf::from("git"))
    } else {
        crate::hosted::git_exe().context("cannot resolve the zvcs binary path")
    }
}

/// Point `link` at `target`, idempotently: a correct symlink is left alone, a
/// stale one is repointed, and a real (non-symlink) file is never clobbered.
pub fn link_to(link: &Path, target: &Path, stats: &mut LinkStats) -> Result<()> {
    match std::fs::symlink_metadata(link) {
        Ok(m) if m.file_type().is_symlink() => {
            if std::fs::read_link(link).ok().as_deref() == Some(target) {
                stats.current += 1;
                return Ok(());
            }
            let _ = std::fs::remove_file(link); // stale target → repoint below
        }
        Ok(_) => {
            stats.skipped += 1; // a real file/dir with this name — leave it untouched
            return Ok(());
        }
        Err(_) => {} // absent — create below
    }
    symlink(target, link).with_context(|| format!("cannot link {}", link.display()))?;
    stats.created += 1;
    Ok(())
}

/// Install a `git-<verb>` symlink into `dir` for every verb the dispatcher
/// serves, pointing at `target`. Creates `dir` if it does not exist.
pub fn install_links(dir: &Path, target: &Path) -> Result<LinkStats> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut stats = LinkStats::default();
    for verb in PORCELAIN_VERBS.iter().chain(SUPERSET_VERBS) {
        link_to(&dir.join(format!("git-{verb}")), target, &mut stats)?;
    }
    Ok(stats)
}

pub fn zdashed(args: &[String]) -> Result<ExitCode> {
    let dir: PathBuf = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::superset::zdaemon::zvcs_home().join("bin"));

    let target = link_target(&dir)?;
    let stats = install_links(&dir, &target)?;

    // Also materialize the superset man pages so `man git-<verb>` resolves once
    // `~/.zvcs/man` is on MANPATH; `git help <zverb>` works regardless. The HTML
    // set goes down beside them, where `git --html-path` reports and
    // `git help -w <cmd>` looks.
    let man = crate::superset::manpage::install_all().unwrap_or(0);
    let man_dir = crate::superset::manpage::man_dir().join("man1");
    let html = crate::superset::htmldoc::install_all().unwrap_or(0);
    let html_dir = crate::superset::htmldoc::html_dir();

    println!(
        "installed {} git-<verb> link(s) in {} ({} already current, {} skipped); {man} man page(s) in {}; {html} HTML page(s) in {}",
        stats.created,
        dir.display(),
        stats.current,
        stats.skipped,
        man_dir.display(),
        html_dir.display()
    );
    Ok(ExitCode::SUCCESS)
}
