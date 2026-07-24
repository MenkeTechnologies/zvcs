//! zvcs ASCII logo + live-stats box banner, shown on `git zrepl` startup.
//!
//! Ported from strykelang's `banner.rs`: the same width-correct renderer (a
//! `visible_width` that ignores ANSI SGR escapes, and a `row` helper that pads
//! each interior line to a fixed inner width so the box never drifts), with
//! zvcs's own logo, stats, and tagline. Every count is pulled at call time —
//! the dispatch tables for verb counts, the ledger for the indexed-repo count —
//! so the banner never goes stale after a `cargo build` adds verbs.

/// Count of visible columns in `s`, ignoring ANSI SGR escape sequences.
/// Multi-byte UTF-8 counts as one column per char — sufficient for the
/// box-drawing glyphs and Latin labels here.
pub fn visible_width(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut w = 0usize;
    while i < bytes.len() {
        if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            while i < bytes.len() && !(0x40..=0x7E).contains(&bytes[i]) {
                i += 1;
            }
            i += 1;
        } else {
            let step = std::str::from_utf8(&bytes[i..])
                .ok()
                .and_then(|s| s.chars().next())
                .map(|c| c.len_utf8())
                .unwrap_or(1);
            w += 1;
            i += step;
        }
    }
    w
}

/// Number of repos in the ledger, or `None` when there is no index yet. Kept
/// cheap (a single `COUNT(*)`, read-only handle) so it never slows repl start.
fn indexed_repo_count() -> Option<usize> {
    if !crate::db::db_path().exists() {
        return None;
    }
    let conn = crate::db::open_ro().ok()?;
    conn.query_row("SELECT COUNT(*) FROM repos", [], |r| r.get::<_, i64>(0))
        .ok()
        .map(|n| n as usize)
}

/// Render the zvcs logo + stats box + tagline into a string. `colored=true`
/// emits ANSI SGR escapes; `false` returns plain text (used by the width tests).
pub fn render_banner(colored: bool) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let n_super = crate::dispatch::SUPERSET_VERBS.len();
    let n_porc = crate::dispatch::PORCELAIN_VERBS.len();
    let n_total = n_super + n_porc;
    let repos = indexed_repo_count();

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let pid = std::process::id();

    let (c, m, r, y, g, n) = if colored {
        (
            "\x1b[36m", "\x1b[35m", "\x1b[31m", "\x1b[33m", "\x1b[32m", "\x1b[0m",
        )
    } else {
        ("", "", "", "", "", "")
    };

    const INNER: usize = 64;
    let mut out = String::with_capacity(2048);

    let row = |out: &mut String, body: &str| {
        let pad = INNER.saturating_sub(visible_width(body));
        out.push_str(&format!("{c} │{n}{body}{:pad$}{c}│{n}\n", "", pad = pad));
    };

    // ZVCS logo — a cyan→magenta→red gradient over the six glyph rows.
    out.push_str(&format!("{c} ███████╗██╗   ██╗ ██████╗███████╗{n}\n"));
    out.push_str(&format!("{c} ╚══███╔╝██║   ██║██╔════╝██╔════╝{n}\n"));
    out.push_str(&format!("{m}   ███╔╝ ██║   ██║██║     ███████╗{n}\n"));
    out.push_str(&format!("{m}  ███╔╝  ╚██╗ ██╔╝██║     ╚════██║{n}\n"));
    out.push_str(&format!("{r} ███████╗ ╚████╔╝ ╚██████╗███████║{n}\n"));
    out.push_str(&format!("{r} ╚══════╝  ╚═══╝   ╚═════╝╚══════╝{n}\n"));

    out.push_str(&format!(
        "{c} ┌────────────────────────────────────────────────────────────────┐{n}\n"
    ));
    row(
        &mut out,
        &format!(
            " {y}SYSTEM{n}  status:{g} ONLINE {c}//{n} {y}os:{n} {os} {y}arch:{n} {arch} {y}pid:{n} {pid}"
        ),
    );
    let repos_cell = match repos {
        Some(k) => format!("{k}"),
        None => "—".to_string(),
    };
    row(
        &mut out,
        &format!(
            " {y}CORES{n}   {cores:<4} {c}//{n} {y}REPOS{n}  {repos_cell} indexed"
        ),
    );
    out.push_str(&format!(
        "{c} ├────────────────────────────────────────────────────────────────┤{n}\n"
    ));
    row(
        &mut out,
        &format!(
            " {y}VERBS{n}  superset {n_super:<4} {c}//{n} git-compat {n_porc:<4} {c}//{n} total {n_total:<4}"
        ),
    );
    out.push_str(&format!(
        "{c} └────────────────────────────────────────────────────────────────┘{n}\n"
    ));
    out.push_str(&format!(
        "{m}  >> GIT-SHADOWING SUPERSET VCS // RUST-POWERED v{version} <<{n}\n"
    ));
    out
}

/// Print the banner to stdout. Convenience wrapper around [`render_banner`].
pub fn print_banner(colored: bool) {
    print!("{}", render_banner(colored));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_width_ignores_csi_sequences() {
        assert_eq!(visible_width("\x1b[31mabc\x1b[0m"), 3);
        assert_eq!(visible_width("\x1b[1;38;5;202mok"), 2);
    }

    #[test]
    fn visible_width_counts_each_char_once_for_multibyte() {
        assert_eq!(visible_width("─├┤"), 3);
        assert_eq!(visible_width("aé你"), 3);
    }

    #[test]
    fn render_banner_plain_has_no_ansi_escapes() {
        let s = render_banner(false);
        assert!(!s.contains('\x1b'), "plain banner must not contain ESC");
        assert!(s.contains("GIT-SHADOWING SUPERSET VCS"));
        assert!(s.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn render_banner_colored_contains_ansi_escapes() {
        let s = render_banner(true);
        assert!(s.contains("\x1b["));
        assert!(s.contains("\x1b[0m"));
    }

    #[test]
    fn render_banner_rows_all_match_inner_width_after_strip() {
        // Anchor the expected width to the top border, then prove every interior
        // row matches it — catches padding drift in `row()` even if the box is
        // retuned later. (The CLAUDE.md box-drawing rule: never eyeball, verify.)
        let s = render_banner(false);
        let top = s
            .lines()
            .find(|l| l.starts_with(" ┌"))
            .expect("top border present");
        let want = visible_width(top);
        let mut box_rows = 0;
        for line in s.lines() {
            if line.starts_with(" │") && line.ends_with('│') {
                box_rows += 1;
                assert_eq!(visible_width(line), want, "box row width drift on line: {line}");
            }
        }
        assert!(box_rows >= 3, "expected several rendered box rows");
    }
}
