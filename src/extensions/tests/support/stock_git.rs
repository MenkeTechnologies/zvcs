//! The stock git a test compares zvcs against — one policy for every test file,
//! the one `src/parity/src/stock.rs` applies to the parity harness.
//!
//! `ZVCS_STOCK_GIT` names it outright. Otherwise it is the **newest** of the
//! fixed install locations that is really git, so the oracle is the release the
//! port targets rather than whichever older git happens to come first — Apple's
//! `/usr/bin/git` lags Homebrew's by several releases, and tests that took the
//! first candidate measured zvcs against git 2.54 behaviour.
//!
//! `PATH` is never consulted: on a machine where zvcs shadows git, a `PATH`
//! lookup silently makes the oracle the binary under test.
//!
//! Each file includes this with
//!
//! ```ignore
//! #[path = "support/stock_git.rs"]
//! mod stock_git;
//! use stock_git::stock_git;
//! ```

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

const CANDIDATES: [&str; 3] = ["/usr/bin/git", "/opt/homebrew/bin/git", "/usr/local/bin/git"];

/// The stock git to compare against, or `None` when this machine has none.
/// Resolved once per test binary.
#[allow(dead_code)]
pub fn stock_git() -> Option<&'static str> {
    static FOUND: OnceLock<Option<String>> = OnceLock::new();
    FOUND.get_or_init(resolve).as_deref()
}

/// [`stock_git`] when it is at least `min`, for a comparison an older git cannot
/// make — it lacks the behaviour under test, so it would "disagree" about
/// something it simply does not have.
#[allow(dead_code)]
pub fn stock_git_at_least(min: (u32, u32, u32)) -> Option<&'static str> {
    stock_git().filter(|bin| version_of(bin).is_some_and(|v| v >= min))
}

fn resolve() -> Option<String> {
    if let Ok(p) = std::env::var("ZVCS_STOCK_GIT") {
        return Path::new(&p).exists().then_some(p);
    }
    CANDIDATES
        .into_iter()
        .filter(|bin| Path::new(bin).exists() && !is_zvcs(bin))
        .filter_map(|bin| Some((version_of(bin)?, bin.to_owned())))
        .max()
        .map(|(_, bin)| bin)
}

/// Whether `bin` is zvcs wearing git's name.
///
/// zvcs serves the superset verb `zverbs` itself; a stock git looks for a
/// `git-zverbs` on `PATH` and fails. The environment is emptied because zvcs's
/// installation puts a `git-zverbs` shim on `PATH`, which a stock git would then
/// answer too. A throwaway `ZVCS_HOME` and a temp working directory keep an old
/// zvcs build from writing its state into the source tree; a stock git ignores
/// both.
fn is_zvcs(bin: &str) -> bool {
    let scratch = std::env::temp_dir().join(format!("zvcs-stockprobe-{}", std::process::id()));
    let answered = Command::new(bin)
        .arg("zverbs")
        .env_clear()
        .env("ZVCS_HOME", &scratch)
        .current_dir(std::env::temp_dir())
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false);
    let _ = std::fs::remove_dir_all(&scratch);
    answered
}

/// `git version X.Y.Z` as a comparable tuple, or `None` when it will not answer.
fn version_of(bin: &str) -> Option<(u32, u32, u32)> {
    let out = Command::new(bin).arg("--version").env_clear().output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let rest = text.trim().strip_prefix("git version ")?;
    let mut parts = rest.split(['.', ' ', '-']).filter_map(|p| p.parse::<u32>().ok());
    Some((parts.next()?, parts.next().unwrap_or(0), parts.next().unwrap_or(0)))
}
