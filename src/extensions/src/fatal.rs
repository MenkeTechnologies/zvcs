//! Errors that are git's, told apart from errors that are this port's.
//!
//! git's `die()` writes `fatal: <message>` to stderr and exits 128. Nearly every
//! diagnostic a caller can provoke — a bad revision, a contradictory pair of
//! options, an unmerged path — arrives in that shape, and callers read it: a
//! client checks the exit code, and some parse the text.
//!
//! This port also has to say things git never says: that a feature is not ported.
//! That is a different claim, and it must not wear git's clothes. `fatal: …` with
//! exit 128 asserts "this is what git does here"; a port that has not implemented
//! something and says so in git's voice is lying about its own coverage, which is
//! worse than the gap it is papering over.
//!
//! So the two are distinct types. [`Fatal`] is the first — a message git itself
//! would `die()` with, rendered exactly as git renders it. An ordinary
//! `anyhow::bail!` is the second, and keeps the `zvcs: <verb>: …` prefix and exit
//! 1 that mark it as this binary speaking for itself. `run_command` in `lib.rs`
//! is the one place that tells them apart.

use std::fmt;

/// A message git would `die()` with: `fatal: <message>` on stderr, exit 128.
#[derive(Debug)]
pub struct Fatal(pub String);

impl fmt::Display for Fatal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Without the prefix: the renderer adds it, and a `Fatal` that ends up
        // inside someone else's context chain should read as the bare message.
        f.write_str(&self.0)
    }
}

impl std::error::Error for Fatal {}

/// git's exit code for `die()`.
pub const EXIT_FATAL: u8 = 128;

/// A diagnostic already written to stderr; only the exit code is left to carry.
///
/// Not every git diagnostic is a `die()`. `report_path_error()` writes `error: …`
/// and lets the caller return 1; `parse_options` writes `error: …` followed by the
/// usage block and exits 129. Those have already said their piece by the time they
/// unwind, so the renderer must add nothing — printing `zvcs: <verb>: …` after
/// them would double the message and change the exit code.
#[derive(Debug)]
pub struct Silent(pub u8);

impl fmt::Display for Silent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("")
    }
}

impl std::error::Error for Silent {}

/// Return early with a message git would `die()` with — `bail!` for git's voice.
///
/// ```ignore
/// git_fatal!("No paths with --include/--only does not make sense.");
/// git_fatal!("could not lookup commit {rev}");
/// ```
#[macro_export]
macro_rules! git_fatal {
    ($($arg:tt)*) => {
        return ::std::result::Result::Err(
            ::anyhow::Error::new($crate::fatal::Fatal(::std::format!($($arg)*)))
        )
    };
}

/// The same message as [`git_fatal!`] as a value, for the places that build an
/// error instead of returning one — `ok_or_else`, `map_err`, and friends.
pub fn die(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Fatal(message.into()))
}

/// git's `setup_work_tree()`: the commands that need a work tree die with this
/// when setup did not find one, which is what standing in a `.git` directory or
/// a bare repository leaves them with.
pub fn need_work_tree() -> anyhow::Error {
    die("this operation must be run in a work tree")
}
