//! `quote.c`: C-style path quoting, and the `core.quotePath` global it reads.
//!
//! git keeps exactly one `cq_lookup[]` table and one `quote_path_fully` global, and
//! every `quote_c_style()` caller in the tree shares them. This module is that pair:
//! the table lives here, the flag lives here, and every verb that prints a path goes
//! through these functions so `git -c core.quotePath=false <anything>` answers the
//! same way `git -c core.quotePath=false <anything else>` does.

use gix::bstr::BString;
use std::sync::OnceLock;

/// git's `quote_path_fully` global (`git_default_core_config()`, default true).
///
/// Seeded by [`init`] from the repository a verb has already opened, and otherwise
/// resolved on first use by [`quote_path_fully`] — so a verb that never calls [`init`]
/// still answers from `core.quotePath` rather than from the default.
static QUOTE_PATH_FULLY: OnceLock<bool> = OnceLock::new();

/// Seed [`QUOTE_PATH_FULLY`] from an open repository's `core.quotePath`.
///
/// Call once, right after the repository is open and before anything is rendered.
/// The first seeding wins, matching git's single `git_config()` pass.
pub fn init(repo: &gix::Repository) {
    let _ = QUOTE_PATH_FULLY.set(read_config(repo));
}

fn read_config(repo: &gix::Repository) -> bool {
    repo.config_snapshot()
        .boolean("core.quotePath")
        .unwrap_or(true)
}

/// `quote_path_fully`. Falls back to discovering the repository when no verb seeded
/// it, which costs one discovery per process and only on the first byte >= 0x80 that
/// is actually rendered. Outside a repository the answer is git's default, true.
pub fn quote_path_fully() -> bool {
    *QUOTE_PATH_FULLY.get_or_init(|| gix::discover(".").as_ref().map_or(true, read_config))
}

/// The escape character for `b`, or `None` if it can be emitted verbatim.
/// `Some(0)` means "octal-escape this byte".
///
/// This is git's `cq_lookup[]` table combined with `cq_must_quote()`: entries the table
/// marks `-1` are never quoted, the named escapes and `"`/`\` are always quoted (their
/// table entries are `>= ' '`, so `quote_path_fully` cannot switch them off), controls
/// and DEL are always octal-escaped, and the high half (table entry `0`) is octal-escaped
/// only while `quote_path_fully` is on.
fn cq_escape(b: u8) -> Option<u8> {
    match b {
        0x07 => Some(b'a'),
        0x08 => Some(b'b'),
        0x09 => Some(b't'),
        0x0a => Some(b'n'),
        0x0b => Some(b'v'),
        0x0c => Some(b'f'),
        0x0d => Some(b'r'),
        b'"' => Some(b'"'),
        b'\\' => Some(b'\\'),
        // Table entry 1: quoted whatever `core.quotePath` says.
        0x00..=0x1f | 0x7f => Some(0),
        // Table entry 0: quoted only while `quote_path_fully` is on.
        0x80..=0xff => quote_path_fully().then_some(0),
        _ => None,
    }
}

/// `quote_c_style(s, NULL, NULL, 0)` used as a predicate: whether quoting would
/// change the string at all.
pub fn needs_c_quote(s: &[u8]) -> bool {
    s.iter().any(|b| cq_escape(*b).is_some())
}

/// The escaped body of `s`, without the surrounding double quotes — git's
/// `quote_c_style_counted()` with `CQUOTE_NODQ`.
pub fn cq_body(s: &[u8], out: &mut Vec<u8>) {
    for &b in s {
        match cq_escape(b) {
            None => out.push(b),
            Some(0) => {
                out.push(b'\\');
                out.push(((b >> 6) & 0o3) + b'0');
                out.push(((b >> 3) & 0o7) + b'0');
                out.push((b & 0o7) + b'0');
            }
            Some(c) => {
                out.push(b'\\');
                out.push(c);
            }
        }
    }
}

/// `write_name_quoted()`: the path, double-quoted and escaped only if needed.
pub fn quoted_name(path: &BString) -> Vec<u8> {
    quoted_name_bytes(path.as_slice())
}

/// [`quoted_name`] over a plain byte slice, for the callers that never hold a `BString`.
pub fn quoted_name_bytes(s: &[u8]) -> Vec<u8> {
    if !needs_c_quote(s) {
        return s.to_vec();
    }
    let mut out = vec![b'"'];
    cq_body(s, &mut out);
    out.push(b'"');
    out
}

/// [`quoted_name_bytes`] for the callers that assemble their output as a `String`.
///
/// A quoted result is pure ASCII, so it converts exactly. An unquoted one is the path
/// itself, which is only ever non-ASCII while `core.quotePath` is off — and then only
/// lossless if the path is valid UTF-8. A path that is neither valid UTF-8 nor quoted
/// (`core.quotePath=false` over, say, a Latin-1 name recorded on Linux) reaches these
/// callers as U+FFFD where git writes the raw byte. Moving one to [`quoted_name_bytes`]
/// removes that for the verb in question; the byte-level callers already have none.
pub fn quoted_name_string(s: &[u8]) -> String {
    match needs_c_quote(s) {
        false => String::from_utf8_lossy(s).into_owned(),
        true => String::from_utf8_lossy(&quoted_name_bytes(s)).into_owned(),
    }
}

/// `quote_two_c_style()`: `<prefix><path>` quoted as a whole when either half needs
/// escaping, so the `a/` of a `--- a/path` line stays inside the quotes.
pub fn quote_two_c_style(prefix: &[u8], path: &[u8]) -> Vec<u8> {
    if !needs_c_quote(prefix) && !needs_c_quote(path) {
        let mut out = prefix.to_vec();
        out.extend_from_slice(path);
        return out;
    }
    let mut out = vec![b'"'];
    cq_body(prefix, &mut out);
    cq_body(path, &mut out);
    out.push(b'"');
    out
}

