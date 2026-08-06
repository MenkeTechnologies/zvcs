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

/// git's `MINIMUM_ABBREV`: the shortest id `--abbrev=<n>` can ask for.
pub const MINIMUM_ABBREV: usize = 4;

/// `--abbrev=<n>`'s value, read as `diff_opt_parse()` reads it: `strtoul()` over the
/// leading digits — a value with none is zero — then clamped to `[MINIMUM_ABBREV,
/// hexsz]`. Nothing here is an error, so `--abbrev=xyz` is the 4-character minimum.
pub fn parse_abbrev_arg(v: &str, hexsz: usize) -> usize {
    let digits: String = v.chars().take_while(char::is_ascii_digit).collect();
    // No digits is `strtoul`'s 0; digits that overflow saturate, as `strtoul`'s
    // `ULONG_MAX` does before the clamp below.
    let n: usize = match digits.is_empty() {
        true => 0,
        false => digits.parse().unwrap_or(usize::MAX),
    };
    n.clamp(MINIMUM_ABBREV, hexsz)
}

/// Auto abbreviation length: `ceil(log2(objects) / 2)`, floored at 7 — the same
/// heuristic `gix` uses for `core.abbrev = auto`.
pub fn auto_abbrev(repo: &gix::Repository, hexsz: usize) -> usize {
    let count = repo.objects.packed_object_count().unwrap_or(0);
    let mut len = (64 - count.leading_zeros()) as usize;
    len = len.div_ceil(2);
    len.max(7).min(hexsz)
}
