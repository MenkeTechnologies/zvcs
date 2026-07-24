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

use crate::dispatch::{PORCELAIN_VERBS, SUPERSET_VERBS};
use anyhow::{Context, Result};
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::ExitCode;

pub fn zdashed(args: &[String]) -> Result<ExitCode> {
    let dir: PathBuf = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::superset::zdaemon::zvcs_home().join("bin"));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create {}", dir.display()))?;

    // Link target: the sibling `git` when it already exists (so the dashed forms
    // track whatever the shim points at, surviving rebuilds), else this binary by
    // absolute path.
    let target: PathBuf = if dir.join("git").exists() {
        PathBuf::from("git")
    } else {
        std::env::current_exe().context("cannot resolve the zvcs binary path")?
    };

    let mut created = 0usize;
    let mut current = 0usize;
    let mut skipped = 0usize;
    for verb in PORCELAIN_VERBS.iter().chain(SUPERSET_VERBS) {
        let link = dir.join(format!("git-{verb}"));
        match std::fs::symlink_metadata(&link) {
            Ok(m) if m.file_type().is_symlink() => {
                if std::fs::read_link(&link).ok().as_deref() == Some(target.as_path()) {
                    current += 1;
                    continue;
                }
                let _ = std::fs::remove_file(&link); // stale target → repoint below
            }
            Ok(_) => {
                skipped += 1; // a real file/dir with this name — leave it untouched
                continue;
            }
            Err(_) => {} // absent — create below
        }
        symlink(&target, &link).with_context(|| format!("cannot link {}", link.display()))?;
        created += 1;
    }

    // Also materialize the superset man pages so `man git-<verb>` resolves once
    // `~/.zvcs/man` is on MANPATH; `git help <zverb>` works regardless.
    let man = crate::superset::manpage::install_all().unwrap_or(0);
    let man_dir = crate::superset::manpage::man_dir().join("man1");

    println!(
        "installed {created} git-<verb> link(s) in {} ({current} already current, {skipped} skipped); {man} man page(s) in {}",
        dir.display(),
        man_dir.display()
    );
    Ok(ExitCode::SUCCESS)
}
