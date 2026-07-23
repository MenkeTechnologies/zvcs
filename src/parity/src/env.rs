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
