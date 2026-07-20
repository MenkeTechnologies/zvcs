//! `git mktree` — build a tree object from non-recursive `ls-tree` text on stdin.
//!
//! Covered: the whole documented surface — `-z`, `--missing` / `--no-missing`,
//! `--batch` / `--no-batch`, `--`, and `-h`. Input parsing follows
//! `builtin/mktree.c` step for step: `<mode> SP <type> SP <oid> TAB <path>`, with
//! the mode read by `strtoul(…, 8)` (leading blanks and a sign are accepted and
//! the value is truncated to 32 bits, exactly as C does), C-style unquoting of a
//! `"…"` path when not in `-z` mode, the redundant mode-type / type-word /
//! object-type agreement checks, and gitlinks being exempt from the odb presence
//! check. Entries are sorted with git's `base_name_compare` (a tree's name
//! compares as if it carried a trailing `/`) and serialized as
//! `%o SP name NUL <raw oid>`, so the tree bytes — and therefore the printed id —
//! are git's.
//!
//! Deliberately faithful quirks: modes are written back verbatim (`644` stays
//! `644`, it is not canonicalized to `100644`), duplicate paths are kept, text
//! after a closing quote is discarded, and a `\r` before `\n` is part of the
//! path because `mktree` reads with `strbuf_getline_lf`, which does not strip CR.
//!
//! Exit codes follow git: 0 on success, 128 for every `fatal:`, 129 for `-h` and
//! usage errors. Positional arguments are accepted and ignored, as git ignores
//! them.
//!
//! One substrate note: the empty tree always reads as present in the odb here
//! because gitoxide's `try_find_header` special-cases it — which matches git,
//! whose object layer registers the empty tree as a cached object.

use anyhow::{anyhow, Result};
use std::cmp::Ordering;
use std::io::{Read, Write as _};
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::objs::{Kind, Write as _};

/// Stock git's `mktree` usage block, byte-for-byte (208 bytes), including the
/// trailing blank line. Printed on `-h` (stdout) and after the `error:` line for
/// a usage error (stderr).
const USAGE: &str = "usage: git mktree [-z] [--missing] [--batch]\n\
                     \n\
                     \x20   -z                    input is NUL terminated\n\
                     \x20   --[no-]missing        allow missing objects\n\
                     \x20   --[no-]batch          allow creation of more than one tree\n\
                     \n";

/// git's exit code for a `die()`.
const FATAL: u8 = 128;

/// One accumulated tree entry, in the shape `append_to_tree` records it.
struct Entry {
    /// The mode exactly as parsed; re-emitted with `%o`, never canonicalized.
    mode: u32,
    oid: ObjectId,
    /// The final (unquoted) path component; never contains `/`.
    name: Vec<u8>,
}

/// `git mktree` — read `ls-tree`-formatted records from stdin and write trees.
///
/// Without `--batch` the whole of stdin is one tree and its id is printed once,
/// even when the input was empty (that yields the empty-tree id). With `--batch`
/// a blank record closes the current tree and starts the next, and a trailing
/// blank record does not create an extra empty tree.
pub fn mktree(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first() {
        Some(a) if a == "mktree" => &args[1..],
        _ => args,
    };

    // git.c answers a lone `-h` before repository setup, so it works anywhere.
    if args.len() == 1 && args[0] == "-h" {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let repo = gix::discover(".")?;

    let mut nul_term_line = false;
    let mut allow_missing = false;
    let mut is_batch_mode = false;
    let mut no_more_opts = false;

    for arg in args {
        let arg = arg.as_str();
        // A bare `-` is a positional to `parse_options`. mktree has no
        // positionals and silently ignores whatever is left over.
        if no_more_opts || arg == "-" || !arg.starts_with('-') {
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            if long.is_empty() {
                no_more_opts = true;
                continue;
            }
            match resolve_long(long) {
                Some((Opt::Missing, value)) => allow_missing = value,
                Some((Opt::Batch, value)) => is_batch_mode = value,
                None => return Ok(usage_error(&format!("unknown option `{long}'"))),
            }
            continue;
        }
        // Short options: `-z` is the only one and it takes no value.
        for c in arg[1..].chars() {
            match c {
                'z' => nul_term_line = true,
                'h' => {
                    print!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
                _ => return Ok(usage_error(&format!("unknown switch `{c}'"))),
            }
        }
    }

    // Read stdin fully before taking the repo lock: a `--batch` producer that
    // stalls must not block other zvcs writers.
    let mut input = Vec::new();
    std::io::stdin().lock().read_to_end(&mut input)?;

    let terminator = if nul_term_line { 0u8 } else { b'\n' };
    let records = split_records(&input, terminator);
    let hex_len = repo.object_hash().len_in_hex();

    // Serialize object writes through the repo coordinator, as the other writing
    // porcelain does.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut stdout = std::io::stdout().lock();
    let mut entries: Vec<Entry> = Vec::new();
    let mut next = 0usize;
    let mut got_eof = false;

    while !got_eof {
        loop {
            let Some(&record) = records.get(next) else {
                got_eof = true;
                break;
            };
            next += 1;

            // C reads each record as a NUL-terminated string, so an embedded
            // `\0` in an LF-delimited line ends it — and a leading one makes the
            // line read as blank.
            let line = &record[..record.iter().position(|&b| b == 0).unwrap_or(record.len())];
            if line.is_empty() {
                // Blank records only delimit trees in batch mode.
                if is_batch_mode {
                    break;
                }
                fatal(b"input format error: (blank line only valid in batch mode)");
                return Ok(ExitCode::from(FATAL));
            }

            match mktree_line(&repo, line, nul_term_line, allow_missing, hex_len)? {
                Some(entry) => entries.push(entry),
                None => return Ok(ExitCode::from(FATAL)),
            }
        }

        // The final terminator is optional, so `--batch` at EOF with nothing
        // pending stops instead of emitting a spurious empty tree.
        if !(is_batch_mode && got_eof && entries.is_empty()) {
            let id = write_tree(&repo, &mut entries)?;
            writeln!(stdout, "{id}")?;
            stdout.flush()?;
        }
        entries.clear();
    }

    Ok(ExitCode::SUCCESS)
}

/// The two long options, resolved past their `no-` form.
#[derive(Clone, Copy)]
enum Opt {
    Missing,
    Batch,
}

/// Match a long option — git's `parse_options` accepts any unambiguous prefix —
/// to the option it names and the value it sets.
fn resolve_long(long: &str) -> Option<(Opt, bool)> {
    const SPELLINGS: [(&str, Opt, bool); 4] = [
        ("missing", Opt::Missing, true),
        ("batch", Opt::Batch, true),
        ("no-missing", Opt::Missing, false),
        ("no-batch", Opt::Batch, false),
    ];

    let mut prefix_hit = None;
    for (spelling, opt, value) in SPELLINGS {
        if spelling == long {
            return Some((opt, value));
        }
        if spelling.starts_with(long) {
            if prefix_hit.is_some() {
                return None; // ambiguous, e.g. `--no`
            }
            prefix_hit = Some((opt, value));
        }
    }
    prefix_hit
}

/// Parse one `ls-tree` record into an entry.
///
/// `Ok(None)` means a `fatal:` has already been reported and the caller must
/// exit 128. The checks run in `mktree_line`'s own order: input format, type
/// word, mode/type agreement, recorded object type, then the slash rule.
fn mktree_line(
    repo: &gix::Repository,
    line: &[u8],
    nul_term_line: bool,
    allow_missing: bool,
    hex_len: usize,
) -> Result<Option<Entry>> {
    // mode SP type SP oid TAB name
    let Some((mode, after_mode)) = parse_octal(line) else {
        return Ok(fatal_format(line));
    };
    if line.get(after_mode) != Some(&b' ') {
        return Ok(fatal_format(line));
    }
    let type_start = after_mode + 1;
    let Some(offset) = line[type_start..].iter().position(|&b| b == b' ') else {
        return Ok(fatal_format(line));
    };
    let type_word = &line[type_start..type_start + offset];

    let oid_start = type_start + offset + 1;
    let oid_end = oid_start + hex_len;
    // The hex must be exactly one hash long and followed by the TAB separator.
    if line.len() <= oid_end || line[oid_end] != b'\t' {
        return Ok(fatal_format(line));
    }
    // git's `parse_oid_hex` accepts either case; gitoxide's decoder wants lower.
    let Ok(oid) = ObjectId::from_hex(&line[oid_start..oid_end].to_ascii_lowercase()) else {
        return Ok(fatal_format(line));
    };

    // A submodule's commit legitimately lives in the submodule's own odb.
    let allow_missing = allow_missing || is_gitlink(mode);

    let raw_path = &line[oid_end + 1..];
    let path = if !nul_term_line && raw_path.first() == Some(&b'"') {
        match unquote_c_style(raw_path) {
            Some(path) => path,
            None => {
                fatal(b"invalid quoting");
                return Ok(None);
            }
        }
    } else {
        raw_path.to_vec()
    };
    // The unquoted path stays a C string, so an escaped NUL terminates it.
    let path = &path[..path.iter().position(|&b| b == 0).unwrap_or(path.len())];

    // The object type is derivable three ways and all three must agree.
    let Some(named_type) = type_from_string(type_word) else {
        let mut msg = b"invalid object type \"".to_vec();
        msg.extend_from_slice(type_word);
        msg.push(b'"');
        fatal(&msg);
        return Ok(None);
    };
    let mode_type = object_type(mode);
    if mode_type != named_type {
        let mut msg = b"entry '".to_vec();
        msg.extend_from_slice(path);
        msg.extend_from_slice(b"' object type (");
        msg.extend_from_slice(type_word);
        msg.extend_from_slice(b") doesn't match mode type (");
        msg.extend_from_slice(kind_name(mode_type).as_bytes());
        msg.push(b')');
        fatal(&msg);
        return Ok(None);
    }

    match repo.try_find_header(oid)? {
        // Missing objects are presumed to be of the right type.
        None if allow_missing => {}
        None => {
            let mut msg = entry_prefix(path, oid);
            msg.extend_from_slice(b" is unavailable");
            fatal(&msg);
            return Ok(None);
        }
        // Present but of the wrong type: the entry could never become correct,
        // so this is fatal even under `--missing`.
        Some(header) if header.kind() != mode_type => {
            let mut msg = entry_prefix(path, oid);
            msg.extend_from_slice(b" is a ");
            msg.extend_from_slice(kind_name(header.kind()).as_bytes());
            msg.extend_from_slice(b" but specified type was (");
            msg.extend_from_slice(type_word);
            msg.push(b')');
            fatal(&msg);
            return Ok(None);
        }
        Some(_) => {}
    }

    // `append_to_tree` only ever records a single path component.
    if path.contains(&b'/') {
        let mut msg = b"path ".to_vec();
        msg.extend_from_slice(path);
        msg.extend_from_slice(b" contains slash");
        fatal(&msg);
        return Ok(None);
    }

    Ok(Some(Entry {
        mode,
        oid,
        name: path.to_vec(),
    }))
}

/// Sort `entries` git's way, serialize them, and write the tree object.
fn write_tree(repo: &gix::Repository, entries: &mut [Entry]) -> Result<ObjectId> {
    entries.sort_by(|a, b| base_name_compare(&a.name, a.mode, &b.name, b.mode));

    let mut buf = Vec::new();
    for entry in entries.iter() {
        buf.extend_from_slice(format!("{:o} ", entry.mode).as_bytes());
        buf.extend_from_slice(&entry.name);
        buf.push(0);
        buf.extend_from_slice(entry.oid.as_slice());
    }
    repo.write_buf(Kind::Tree, &buf)
        .map_err(|e| anyhow!("unable to write tree object: {e}"))
}

/// git's `base_name_compare`: plain unsigned byte order, except that a
/// directory's name compares as though it ended in `/`.
fn base_name_compare(n1: &[u8], m1: u32, n2: &[u8], m2: u32) -> Ordering {
    let common = n1.len().min(n2.len());
    match n1[..common].cmp(&n2[..common]) {
        Ordering::Equal => {}
        other => return other,
    }
    trailing_byte(n1, common, m1).cmp(&trailing_byte(n2, common, m2))
}

/// The byte just past the common prefix: the next real byte, or the implicit
/// terminator (`/` for a tree, `NUL` otherwise).
fn trailing_byte(name: &[u8], common: usize, mode: u32) -> u8 {
    match name.get(common) {
        Some(&byte) => byte,
        None if is_dir(mode) => b'/',
        None => 0,
    }
}

/// `strtoul(s, &end, 8)` over bytes: leading blanks, an optional sign, then
/// octal digits. Yields the 32-bit-truncated value and the stop offset, or
/// `None` when no digits were converted (C leaves `end == s` in that case).
fn parse_octal(s: &[u8]) -> Option<(u32, usize)> {
    let mut i = 0;
    while matches!(s.get(i), Some(&(b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'))) {
        i += 1;
    }
    let negative = match s.get(i) {
        Some(&b'-') => {
            i += 1;
            true
        }
        Some(&b'+') => {
            i += 1;
            false
        }
        _ => false,
    };

    let digits_start = i;
    let mut value: u64 = 0;
    while let Some(&digit) = s.get(i) {
        if !(b'0'..=b'7').contains(&digit) {
            break;
        }
        // strtoul saturates at ULONG_MAX; the caller's `unsigned` truncates.
        value = value.saturating_mul(8).saturating_add(u64::from(digit - b'0'));
        i += 1;
    }
    if i == digits_start {
        return None;
    }
    let value = if negative { 0u64.wrapping_sub(value) } else { value };
    Some((value as u32, i))
}

/// `type_from_string`, minus the die: the four object type words git knows.
fn type_from_string(word: &[u8]) -> Option<Kind> {
    match word {
        b"commit" => Some(Kind::Commit),
        b"tree" => Some(Kind::Tree),
        b"blob" => Some(Kind::Blob),
        b"tag" => Some(Kind::Tag),
        _ => None,
    }
}

/// git's `object_type(mode)`: only `S_IFDIR` and `S_IFGITLINK` are special, so
/// every other mode — including nonsense like `644` — reads as a blob.
fn object_type(mode: u32) -> Kind {
    if is_dir(mode) {
        Kind::Tree
    } else if is_gitlink(mode) {
        Kind::Commit
    } else {
        Kind::Blob
    }
}

/// `S_ISDIR`: the format bits are `040000`.
fn is_dir(mode: u32) -> bool {
    mode & 0o170000 == 0o040000
}

/// `S_ISGITLINK`: the format bits are `160000`.
fn is_gitlink(mode: u32) -> bool {
    mode & 0o170000 == 0o160000
}

/// The object type word as git spells it in messages.
fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Commit => "commit",
        Kind::Tree => "tree",
        Kind::Blob => "blob",
        Kind::Tag => "tag",
    }
}

/// The shared `entry '<path>' object <oid>` opening of the two odb complaints.
fn entry_prefix(path: &[u8], oid: ObjectId) -> Vec<u8> {
    let mut msg = b"entry '".to_vec();
    msg.extend_from_slice(path);
    msg.extend_from_slice(b"' object ");
    msg.extend_from_slice(oid.to_hex().to_string().as_bytes());
    msg
}

/// git's `unquote_c_style` with a `NULL` end pointer: decode the `"…"` at the
/// front of `quoted` and discard whatever follows the closing quote.
///
/// `None` on a malformed escape or a missing closing quote.
fn unquote_c_style(quoted: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(quoted.len());
    let mut i = 1; // skip the opening quote, which the caller checked

    loop {
        match *quoted.get(i)? {
            b'"' => return Some(out),
            b'\\' => i += 1,
            byte => {
                out.push(byte);
                i += 1;
                continue;
            }
        }

        let escaped = *quoted.get(i)?;
        i += 1;
        out.push(match escaped {
            b'a' => 0x07,
            b'b' => 0x08,
            b'f' => 0x0c,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 0x0b,
            b'\\' | b'"' => escaped,
            // An octal escape is always exactly three digits, and the value is
            // truncated to a byte (`\777` becomes `0xff`).
            b'0'..=b'7' => {
                let mut value = u32::from(escaped - b'0') << 6;
                for shift in [3u32, 0] {
                    let digit = *quoted.get(i)?;
                    if !(b'0'..=b'7').contains(&digit) {
                        return None;
                    }
                    value |= u32::from(digit - b'0') << shift;
                    i += 1;
                }
                value as u8
            }
            _ => return None,
        });
    }
}

/// Split `data` on `terminator`, dropping the empty tail a final terminator
/// leaves behind — the record sequence `strbuf_getline_*` produces, including
/// its acceptance of an unterminated last record.
fn split_records(data: &[u8], terminator: u8) -> Vec<&[u8]> {
    let mut records = Vec::new();
    let mut start = 0;
    while start < data.len() {
        match data[start..].iter().position(|&b| b == terminator) {
            Some(offset) => {
                records.push(&data[start..start + offset]);
                start += offset + 1;
            }
            None => {
                records.push(&data[start..]);
                break;
            }
        }
    }
    records
}

/// `die("input format error: %s", buf)`, shaped for a `mktree_line` return.
fn fatal_format(line: &[u8]) -> Option<Entry> {
    let mut msg = b"input format error: ".to_vec();
    msg.extend_from_slice(line);
    fatal(&msg);
    None
}

/// Report a git `fatal:` on stderr with the message bytes verbatim, so paths
/// that are not valid UTF-8 come out exactly as git prints them.
fn fatal(msg: &[u8]) {
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(b"fatal: ");
    let _ = err.write_all(msg);
    let _ = err.write_all(b"\n");
    let _ = err.flush();
}

/// git's parse-options failure shape: `error: <msg>` then the usage block on
/// stderr, exit 129.
fn usage_error(msg: &str) -> ExitCode {
    eprint!("error: {msg}\n{USAGE}");
    ExitCode::from(129)
}
