//! git's `advice.*` hint gating.
//!
//! Every optional hint git prints (the `hint:` lines that suggest a next step)
//! is controlled by an `advice.<slot>` boolean that defaults to true; setting it
//! false suppresses just that hint. git reads these via `advice_enabled()`; this
//! is the shared gate so every zvcs hint site honors the same switch identically
//! rather than advertising `advice.<slot>` while ignoring it.

/// Whether the `advice.<slot>` hint should be shown: true unless the user set
/// `advice.<slot> = false`. Outside a repository (or when config can't be read)
/// hints show, matching git's default.
pub fn enabled(slot: &str) -> bool {
    match gix::discover(".") {
        Ok(repo) => repo.config_snapshot().boolean(&format!("advice.{slot}")) != Some(false),
        Err(_) => true,
    }
}
