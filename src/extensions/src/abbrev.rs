//! Object-name abbreviation length, resolved the way git resolves it.
//!
//! git abbreviates object ids in `diff` `index` lines, `blame`/`annotate`
//! output, `log --oneline`, and elsewhere to the length named by `core.abbrev`
//! (default `auto`). This is the single shared resolver so every command agrees
//! on the length — a hardcoded `7` diverges from any user whose config sets
//! `core.abbrev` (e.g. `core.abbrev = 10`).

use gix::bstr::ByteSlice;

/// git's effective `core.abbrev`: an explicit number, `auto`/absent → derived
/// from the object count, or `no`/`off`/`false` → the full hash length.
pub fn configured_abbrev(repo: &gix::Repository, hexsz: usize) -> usize {
    match repo
        .config_snapshot()
        .string("core.abbrev")
        .as_ref()
        .and_then(|v| v.to_str().ok().map(str::to_ascii_lowercase))
    {
        None => auto_abbrev(repo, hexsz),
        Some(v) => match v.as_str() {
            "auto" => auto_abbrev(repo, hexsz),
            "no" | "off" | "false" => hexsz,
            other => other
                .parse::<usize>()
                .unwrap_or_else(|_| auto_abbrev(repo, hexsz)),
        },
    }
}

/// Auto abbreviation length: `ceil(log2(objects) / 2)`, floored at 7 — the same
/// heuristic `gix` uses for `core.abbrev = auto`.
pub fn auto_abbrev(repo: &gix::Repository, hexsz: usize) -> usize {
    let count = repo.objects.packed_object_count().unwrap_or(0);
    let mut len = (64 - count.leading_zeros()) as usize;
    len = len.div_ceil(2);
    len.max(7).min(hexsz)
}
