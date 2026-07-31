//! Finding the *stock* git to measure against.
//!
//! Every number this crate produces is a comparison against real git: the
//! fixtures are built with it, each case is run against it, and the report's
//! denominators (`--list-cmds=main`, `git <cmd> -h`, `git help --config`) are
//! read out of it. All of that was resolved as the bare name `git`, through
//! `PATH`.
//!
//! On the machine this is developed on, `PATH` finds `~/.zvcs/bin/git` — zvcs
//! itself, which shadows git deliberately and reports `git version 2.55.0` to be
//! indistinguishable. So the oracle *was the thing under test*: every case
//! compared zvcs with zvcs and matched by construction, and the report measured
//! zvcs's surface against its own. A harness that cannot fail is worse than no
//! harness, because its output still reads like evidence.
//!
//! So the binary is resolved explicitly here, and *verified*: `ZVCS_STOCK_GIT`
//! when set, else the newest of the usual install locations, with each candidate
//! probed before it is trusted. Nothing falls back to a silent `PATH` lookup —
//! when no stock git can be found, the caller is told rather than handed zvcs.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Where a real git usually lives. Order does not matter: the newest of them
/// wins (see [`git_path`]).
const CANDIDATES: [&str; 3] = ["/usr/bin/git", "/opt/homebrew/bin/git", "/usr/local/bin/git"];

/// `git version X.Y.Z` as a comparable tuple, or `None` when it will not answer.
///
/// A machine usually has more than one git — an OS-vendored one under `/usr/bin`
/// and a current one from a package manager — and the port targets the *newest*
/// one it can find. Measuring against the older one silently reports parity with
/// a git nobody is running: `/usr/bin/git` here is 2.50.1 while the port tracks
/// 2.55.0, and the two disagree about real behaviour.
fn version_of(bin: &Path) -> Option<(u32, u32, u32)> {
    let out = Command::new(bin).arg("--version").env_clear().output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let rest = text.trim().strip_prefix("git version ")?;
    let mut parts = rest.split(['.', ' ', '-']).filter_map(|p| p.parse::<u32>().ok());
    Some((parts.next()?, parts.next().unwrap_or(0), parts.next().unwrap_or(0)))
}

/// Whether `bin` is zvcs wearing git's name.
///
/// The probe is a superset verb, run with an emptied environment: zvcs serves
/// `zverbs` itself, while git looks for a `git-zverbs` on `PATH` and says
/// `'zverbs' is not a git command`. Clearing the environment is what makes the
/// probe sound — zvcs's own installation puts a `git-zverbs` shim on `PATH`, so
/// with it a *stock* git answers the verb too.
fn is_zvcs(bin: &Path) -> bool {
    Command::new(bin)
        .arg("zverbs")
        .env_clear()
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

/// The stock git binary, or `None` when the machine has none that is not zvcs.
pub fn git_path() -> Option<&'static Path> {
    static RESOLVED: OnceLock<Option<PathBuf>> = OnceLock::new();
    RESOLVED
        .get_or_init(|| {
            if let Some(explicit) = std::env::var_os("ZVCS_STOCK_GIT") {
                let path = PathBuf::from(explicit);
                // An explicit choice is honoured even if the probe dislikes it:
                // the caller named a binary, and second-guessing that would make
                // the escape hatch useless. It still has to exist.
                return path.exists().then_some(path);
            }
            CANDIDATES
                .iter()
                .map(PathBuf::from)
                .filter(|p| p.exists() && !is_zvcs(p))
                .filter_map(|p| version_of(&p).map(|v| (v, p)))
                .max_by_key(|(v, _)| *v)
                .map(|(_, p)| p)
        })
        .as_deref()
}

/// The stock git binary, or an error naming what to do about its absence.
pub fn git() -> anyhow::Result<&'static Path> {
    git_path().ok_or_else(|| {
        anyhow::anyhow!(
            "no stock git found to measure against (tried {}); \
             set ZVCS_STOCK_GIT to one. `git` on PATH is not used: it is zvcs on \
             any machine where the shadow is installed, and comparing zvcs with \
             itself measures nothing",
            CANDIDATES.join(", ")
        )
    })
}

/// A [`Command`] for the stock git, or an error when there is none.
pub fn command() -> anyhow::Result<Command> {
    Ok(Command::new(git()?))
}
