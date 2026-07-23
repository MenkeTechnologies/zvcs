//! `git get-tar-commit-id` — recover the commit id `git archive` stamped into a tar.
//!
//! A faithful port of `builtin/get-tar-commit-id.c`. The command takes no
//! options and no repository: it reads exactly 1024 bytes (two 512-byte tar
//! records) from stdin, checks that the first record is a pax *global* extended
//! header (`typeflag == 'g'`), and parses the second record as a pax record of
//! the form `<len> comment=<hex><LF>`. On success the `<hex><LF>` tail is
//! written to stdout verbatim; when the archive carries no commit id the
//! command is silent and exits 1.
//!
//! ### Covered (byte-identical stdout and exit code against stock git)
//!
//! * The success path — sha1 (41-byte payload) and sha256 (65-byte payload)
//!   archives alike. The payload length comes from the record's own `<len>`
//!   field, not from any repository's hash algorithm, so a sha256 archive is
//!   decoded correctly while standing in a sha1 repository or outside a
//!   repository entirely.
//! * Exit 1, with no output, for every "no commit id here" shape git
//!   recognises: a non-`g` typeflag (e.g. `git archive` of a bare tree), a
//!   second record whose leading `strtol` conversion fails or overflows or is
//!   negative, a missing `" comment="` separator, and a residual length that is
//!   not `2 * rawsz + 1` for a hash size git knows (20 or 32 bytes).
//! * Exit 128 on a short read or an EOF before 1024 bytes, matching
//!   `read_in_full()` + `die_errno()`.
//! * `-h` and `--help-all`, alone, print the usage line on **stdout** and exit
//!   129 (`show_usage_if_asked`); any other argument prints the same line on
//!   **stderr** and exits 129 (`usage()`). Note that git's master branch has
//!   since changed the `-h` exit to 0 — this module tracks the shipped 2.55
//!   behaviour of exit 129.
//!
//! ### Honest limitations
//!
//! * The `fatal:` lines omit the trailing `: <strerror>` that `die_errno()`
//!   appends. In the EOF case git reports a stale, meaningless `errno` there
//!   (no syscall failed), so the suffix is not reproducible across platforms.
//! * C's `strtol` runs off the end of the 512-byte content record when that
//!   record contains no NUL byte, and C's final `write` can read past the
//!   1024-byte buffer when a pathological digit run pushes the payload beyond
//!   it. Both are out-of-bounds reads in git. This port bounds both to the
//!   record and returns 1 instead; only inputs that are undefined behaviour in
//!   git can tell the difference.

use anyhow::Result;
use std::io::{Read, Write};
use std::process::ExitCode;

/// One tar record.
const RECORDSIZE: usize = 512;
/// ustar header + extended global header content — all this command ever reads.
const HEADERSIZE: usize = 2 * RECORDSIZE;
/// Byte offset of `struct ustar_header.typeflag` (after name/mode/uid/gid/size/mtime/chksum).
const TYPEFLAG_OFFSET: usize = 156;
/// `TYPEFLAG_GLOBAL_HEADER` from `tar.h`.
const TYPEFLAG_GLOBAL_HEADER: u8 = b'g';
/// The pax keyword `git archive` writes the commit id under.
const COMMENT_PREFIX: &[u8] = b" comment=";

/// Stock git's usage line, byte-for-byte. Stdout on `-h`, stderr on a usage error.
const USAGE: &str = "usage: git get-tar-commit-id\n";

/// `git get-tar-commit-id` — extract the commit id from a `git archive` tar on stdin.
pub fn get_tar_commit_id(args: &[String]) -> Result<ExitCode> {
    // show_usage_if_asked(): `-h` / `--help-all` as the sole argument.
    if args.len() == 1 && (args[0] == "-h" || args[0] == "--help-all") {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }
    // usage(): the command takes no arguments at all.
    if !args.is_empty() {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let mut buffer = [0u8; HEADERSIZE];
    match read_in_full(&mut buffer) {
        Err(e) => {
            eprintln!("fatal: git get-tar-commit-id: read error: {e}");
            return Ok(ExitCode::from(128));
        }
        Ok(n) if n != HEADERSIZE => {
            eprintln!("fatal: git get-tar-commit-id: EOF before reading tar header");
            return Ok(ExitCode::from(128));
        }
        Ok(_) => {}
    }

    if buffer[TYPEFLAG_OFFSET] != TYPEFLAG_GLOBAL_HEADER {
        return Ok(ExitCode::FAILURE);
    }

    let content = &buffer[RECORDSIZE..];
    let Some((comment_offset, payload_len)) = parse_comment_record(content) else {
        return Ok(ExitCode::FAILURE);
    };

    let payload = &content[comment_offset..comment_offset + payload_len];
    if let Err(e) = std::io::stdout().write_all(payload) {
        eprintln!("fatal: git get-tar-commit-id: write error: {e}");
        return Ok(ExitCode::from(128));
    }
    Ok(ExitCode::SUCCESS)
}

/// `read_in_full(0, buffer, HEADERSIZE)` — loop until the buffer is full or EOF,
/// returning how many bytes were actually read.
fn read_in_full(buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut stdin = std::io::stdin().lock();
    let mut filled = 0;
    while filled < buffer.len() {
        match stdin.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Parse the extended-header record `<len> comment=<hex><LF>`.
///
/// Returns the offset of the payload within `content` and its length, or `None`
/// for every shape git answers with exit 1. `<len>` counts the whole record, so
/// the payload length is `<len>` minus the bytes the `<len> comment=` prefix
/// itself occupies; git accepts it only when it is `2 * rawsz + 1` for a hash
/// size it knows, which is how the sha1/sha256 distinction is made without
/// consulting a repository.
fn parse_comment_record(content: &[u8]) -> Option<(usize, usize)> {
    // C treats the record as a NUL-terminated string; `git archive` pads with NUL.
    let text = match content.iter().position(|&b| b == 0) {
        Some(nul) => &content[..nul],
        None => content,
    };

    let (len, end) = strtol10(text)?;
    // `end == nptr` means strtol converted nothing.
    if end == 0 || len < 0 {
        return None;
    }
    if !text[end..].starts_with(COMMENT_PREFIX) {
        return None;
    }
    let comment_offset = end + COMMENT_PREFIX.len();

    let payload_len = len - comment_offset as i64;
    // Odd length: the hex digits plus the record's trailing newline.
    if payload_len < 1 || payload_len % 2 == 0 {
        return None;
    }
    // hash_algo_by_length(): git knows only sha1 (20) and sha256 (32).
    if !matches!((payload_len - 1) / 2, 20 | 32) {
        return None;
    }

    let payload_len = payload_len as usize;
    // C reads past its buffer here for pathological input; refuse instead.
    if comment_offset + payload_len > content.len() {
        return None;
    }
    Some((comment_offset, payload_len))
}

/// `strtol(s, &end, 10)` — leading whitespace, an optional sign, then digits.
///
/// Returns the value and the offset of the first unconsumed byte, or `None` when
/// the value would overflow (C's `ERANGE`, which the caller also treats as
/// "no commit id"). When no digits are present the offset is 0, mirroring C's
/// `*end = nptr` so the caller's `end == content` test fires.
fn strtol10(s: &[u8]) -> Option<(i64, usize)> {
    let mut i = 0;
    // C's isspace(): Rust's is_ascii_whitespace omits the vertical tab.
    while i < s.len() && (s[i].is_ascii_whitespace() || s[i] == 0x0b) {
        i += 1;
    }
    let negative = match s.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };

    let digits_start = i;
    let mut value: i64 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(i64::from(s[i] - b'0')))?;
        i += 1;
    }
    if i == digits_start {
        return Some((0, 0));
    }
    Some((if negative { -value } else { value }, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the 512-byte content record git would parse.
    fn record(text: &str) -> Vec<u8> {
        let mut v = vec![0u8; RECORDSIZE];
        v[..text.len()].copy_from_slice(text.as_bytes());
        v
    }

    const SHA1: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn accepts_sha1_and_sha256_records() {
        // 52 = "52 comment=" (11) + 40 hex + LF.
        let c = record(&format!("52 comment={SHA1}\n"));
        assert_eq!(parse_comment_record(&c), Some((11, 41)));

        let sha256 = "a".repeat(64);
        let c = record(&format!("76 comment={sha256}\n"));
        assert_eq!(parse_comment_record(&c), Some((11, 65)));
    }

    #[test]
    fn rejects_lengths_that_name_no_known_hash() {
        // Verified against stock git 2.55: each of these exits 1.
        for text in [
            &format!("41 comment={SHA1}\n"),   // payload 30 — even
            &format!("54 comment={SHA1}\n"),   // payload 43 — 21-byte hash
            &format!("0052 comment={SHA1}\n"), // payload 39 — 19-byte hash
            &format!(" 52 comment={SHA1}\n"),  // leading space widens the prefix
            &format!("xx comment={SHA1}\n"),   // no conversion at all
            &format!("52 comment{SHA1}\n"),    // missing separator
            &format!("-52 comment={SHA1}\n"),  // negative length
        ] {
            assert_eq!(parse_comment_record(&record(text)), None, "{text:?}");
        }
    }

    #[test]
    fn overflowing_length_is_erange_not_a_panic() {
        let c = record(&format!("99999999999999999999999 comment={SHA1}\n"));
        assert_eq!(parse_comment_record(&c), None);
    }

    #[test]
    fn payload_is_not_validated_as_hex() {
        // git checks the length only, so a non-hex comment still round-trips.
        let c = record(&format!("52 comment={}\n", "Z".repeat(40)));
        assert_eq!(parse_comment_record(&c), Some((11, 41)));
    }
}
