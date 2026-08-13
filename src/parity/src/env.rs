//! Hermetic environment for both sides of a differential run.
//!
//! Every source of nondeterminism git consults is pinned here: identity,
//! timestamps, locale, terminal, pager, and the three config scopes. Without
//! this, the user's own `~/.gitconfig` leaks into the "stock" side and the
//! comparison measures the machine instead of the implementation.

use std::path::Path;
use std::process::Command;

/// Fixed identity + clock so commit ids are reproducible across runs and sides.
pub const AUTHOR_NAME: &str = "zvcs parity";
pub const AUTHOR_EMAIL: &str = "parity@example.invalid";
pub const FIXED_DATE: &str = "1700000000 +0000";

/// Apply the hermetic environment to `cmd`, rooted at `home` for config lookup.
///
/// `GIT_CONFIG_{GLOBAL,SYSTEM}` point at `/dev/null` rather than being unset:
/// unsetting them makes git fall back to the real `$HOME`, which is exactly the
/// leak this is closing.
pub fn harden(cmd: &mut Command, home: &Path) {
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", AUTHOR_NAME)
        .env("GIT_AUTHOR_EMAIL", AUTHOR_EMAIL)
        .env("GIT_COMMITTER_NAME", AUTHOR_NAME)
        .env("GIT_COMMITTER_EMAIL", AUTHOR_EMAIL)
        .env("GIT_AUTHOR_DATE", FIXED_DATE)
        .env("GIT_COMMITTER_DATE", FIXED_DATE)
        // Deterministic rendering: no color, no pager, no terminal probing.
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "true")
        // Neutralize every interactive hook. Without these, a mutating command
        // that opens an editor blocks forever, which is why mutating verbs were
        // previously unfuzzable; `true` accepts the supplied message and exits 0.
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("EDITOR", "true")
        .env("VISUAL", "true")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("TZ", "UTC");
}

/// True when `key` is one of the variables [`harden`] pins.
///
/// A case may carry *extra* environment (see `Case::with_env`), and the whole
/// point of `harden` is that the two sides differ in nothing but the binary. So
/// extra environment is allowed to **add** variables git consults and never to
/// replace one of the pins: re-pointing `HOME`, `GIT_CONFIG_GLOBAL` or
/// `GIT_COMMITTER_DATE` from a case would put the machine's own state, or a
/// clock, back into the comparison one case at a time — which is precisely the
/// leak this module exists to close.
///
/// The three variables the discovery cases actually need — `GIT_DIR`,
/// `GIT_WORK_TREE`, `GIT_CEILING_DIRECTORIES` — are *not* pinned, and cannot be:
/// `harden` starts from [`Command::env_clear`], so they are guaranteed absent on
/// both sides unless a case sets them. Setting one is therefore purely additive
/// and stays symmetric, which is what makes repository *discovery* measurable at
/// all.
///
/// Derived from `harden` itself by the test below rather than typed twice, so a
/// new pin cannot silently become overridable.
pub fn is_pinned(key: &str) -> bool {
    pinned_keys().iter().any(|k| k == key)
}

/// Every variable [`harden`] sets, read back off a `Command` it has hardened.
///
/// Asked of `harden` rather than duplicated as a literal list: a duplicate goes
/// stale the first time a pin is added, and a stale list is a hole in the rule
/// above that nothing would report.
pub fn pinned_keys() -> Vec<String> {
    let mut cmd = Command::new("true");
    harden(&mut cmd, Path::new("/nonexistent"));
    cmd.get_envs()
        .filter_map(|(k, v)| v.map(|_| k.to_string_lossy().into_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pin set is non-empty, contains the identity/config/locale pins, and
    /// is what `is_pinned` answers from — so the guard in the runner cannot be
    /// checking against an empty list and passing everything.
    #[test]
    fn pins_are_read_back_from_harden() {
        let keys = pinned_keys();
        for expected in [
            "HOME",
            "PATH",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_AUTHOR_DATE",
            "GIT_COMMITTER_DATE",
            "LC_ALL",
            "TZ",
        ] {
            assert!(keys.iter().any(|k| k == expected), "{expected} not pinned by harden");
            assert!(is_pinned(expected));
        }
    }

    /// The variables that select a repository are deliberately *not* pinned:
    /// `harden` clears the environment, so a case that sets one is adding a fact
    /// both sides see rather than overriding a determinism guarantee.
    #[test]
    fn repository_selection_vars_are_not_pinned() {
        for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_CEILING_DIRECTORIES", "GIT_COMMON_DIR"] {
            assert!(!is_pinned(key), "{key} must stay settable by a case");
        }
    }
}
