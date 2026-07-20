//! `git stage` — a pure synonym for [`git add`](super::add).
//!
//! Stock git implements `stage` as an alias entry in its command table pointing
//! at the very same `cmd_add` C function (`git-stage(1)`: "This is a synonym for
//! git-add(1)"). There is no distinct option parsing, no distinct output, and no
//! distinct exit-code behavior — so this port forwards the argument vector
//! verbatim to the ported [`add`](super::add::add) rather than duplicating its
//! logic, keeping the two permanently in lockstep.
//!
//! Coverage is therefore exactly whatever `add` covers today:
//!   * `git stage <pathspec>...`  — stage files/dirs (recurses, honors `.gitignore`)
//!   * `git stage -A|--all`       — stage the whole worktree (adds, mods, deletes)
//!   * `git stage -u|--update`    — restage tracked paths only (mods + deletes)
//!   * flags `-f/--force`, `-n/--dry-run`, `-v/--verbose`, `--`, and bundled shorts
//!
//! Unsupported flags (`-p`, `-i`, `-e`, `-N`, `--chmod`, `--renormalize`, …) are
//! rejected by `add`'s own parser with its precise message; nothing is silently
//! ignored here. Note that those messages name `git add`, matching stock git,
//! which likewise reports `add` usage for a bad `git stage` invocation.

use anyhow::Result;
use std::process::ExitCode;

/// Forward to the ported `git add`. `args` carries the flags and pathspecs only
/// (the subcommand is stripped by `dispatch::run`, see `lib.rs`), so it is passed
/// straight through without reslicing.
pub fn stage(args: &[String]) -> Result<ExitCode> {
    super::add::add(args)
}
