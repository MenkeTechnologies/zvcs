//! `git mailsplit` — split an mbox or a Maildir into numbered message files.
//!
//! A faithful port of `builtin/mailsplit.c` (git 2.55.0). The command touches no
//! repository state and needs no gitoxide substrate at all: it is pure byte
//! plumbing over stdio and the filesystem, so every path below is a direct
//! translation of the C rather than a re-derivation.
//!
//! ### Covered (byte-identical stdout, stderr, exit code and output files)
//!
//! * `-o<directory>` (attached value only, exactly as the C parser requires),
//!   `-d<prec>` (3..=9, else usage), `-f<n>` (skip the first `n` numbers), `-b`
//!   (allow a bare message with no `From ` envelope line), `--keep-cr`,
//!   `--mboxrd` (un-escape `^>+From `), and `--` as end-of-options.
//! * The backwards-compatible positional forms accepted when `-o` is absent:
//!   `<dir>` (read stdin) and `<mbox> <dir>`; any other count is a usage error.
//! * Option scanning stops at the first argument that does not begin with `-`,
//!   so options written after a filename are treated as filenames — the same
//!   quirk the C loop has. A bare `-` in the option position is therefore
//!   `fatal: unknown option: -`; `-` only means stdin once options have ended.
//! * mbox splitting: the leading-whitespace skip (those bytes are consumed and
//!   never written), `is_from_line()`'s exact backwards colon scan with its
//!   digit and year checks, the `\r\n` → `\n` rewrite, mboxrd `>`-stripping, and
//!   the empty-input rule where empty stdin yields `0` while an empty file is
//!   `error: empty mbox: '<f>'`.
//! * Maildir splitting: `cur` then `new`, dotfiles skipped, a missing subdir
//!   ignored, entries ordered by `maildir_filename_cmp` (digit runs compared as
//!   integers) and de-duplicated on a zero comparison exactly as
//!   `string_list_insert` does — so `01` and `1` collide, as they do in git.
//! * The diagnostics and exit codes: `-h` alone prints the usage line on
//!   **stdout** and exits 129; a usage error prints the same line on stderr and
//!   exits 129; `fatal: unknown option: <arg>` and
//!   `fatal: unable to create '<path>': <errno>` exit 128; `corrupt mailbox`
//!   (unprefixed) exits 1; the `error:`-prefixed input failures exit 1. On
//!   success the number of messages written is printed to stdout.
//!
//! ### Deviations, both in paths where the C is undefined behaviour
//!
//! * git calls `isatty(fileno(f))` *before* checking `f` for NULL, so a mailbox
//!   that fails to open dereferences NULL. Here the
//!   `warning: reading patches from stdin/tty...` probe runs only on a stream
//!   that opened successfully.
//! * `strtol` overflow saturates rather than clamping at `LONG_MAX`; the `-d`
//!   and `-f` results are then truncated to 32 bits, as the C `int` assignment
//!   does on the platforms git supports.

use anyhow::Result;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

/// `git_mailsplit_usage`, byte-for-byte.
const USAGE: &str = "git mailsplit [-d<prec>] [-f<n>] [-b] [--keep-cr] -o<directory> \
                     [(<mbox>|<Maildir>)...]";

/// Control-flow abort standing in for git's `die()` / `usage()` / `exit()`: the
/// diagnostic has already been written, only the exit code still has to travel.
struct Halt(u8);

type R<T> = std::result::Result<T, Halt>;

/// The two flags `mailsplit.c` keeps in file-scope statics.
struct Opts {
    keep_cr: bool,
    mboxrd: bool,
}

/// `git mailsplit` — simple UNIX mbox splitter.
///
/// Never returns `Err`: every failure mode of the C program pairs a specific
/// diagnostic with a specific exit code, so all of them are reported here and
/// surfaced as an [`ExitCode`].
pub fn mailsplit(args: &[String]) -> Result<ExitCode> {
    Ok(match run(args) {
        Ok(code) => code,
        Err(Halt(code)) => ExitCode::from(code),
    })
}

/// `cmd_mailsplit()`.
fn run(args: &[String]) -> R<ExitCode> {
    // show_usage_if_asked(): `-h` as the sole argument, and nothing else.
    if args.len() == 1 && args[0] == "-h" {
        println!("usage: {USAGE}");
        return Err(Halt(129));
    }

    let mut nr: i32 = 0;
    let mut nr_prec: i32 = 4;
    let mut num: i32 = 0;
    let mut allow_bare = false;
    let mut out_dir: Option<String> = None;
    let mut opts = Opts {
        keep_cr: false,
        mboxrd: false,
    };

    // The option loop stops at the first argument not starting with '-'. `idx`
    // is C's `argp`, expressed as an index into `args` (which is `argv + 1`).
    let mut idx = 0;
    while idx < args.len() {
        let arg = args[idx].as_bytes();
        if arg.first() != Some(&b'-') {
            break;
        }
        // Arm order is the C `if`/`else if` chain's order and must stay that way:
        // `-bx`, for instance, falls all the way through to the `die`.
        match arg.get(1) {
            Some(b'd') => {
                nr_prec = strtol(&arg[2..]) as i32;
                if !(3..10).contains(&nr_prec) {
                    return Err(usage());
                }
            }
            Some(b'f') => nr = strtol(&arg[2..]) as i32,
            Some(b'b') if arg.len() == 2 => allow_bare = true,
            _ if args[idx] == "--keep-cr" => opts.keep_cr = true,
            Some(b'o') if arg.len() > 2 => out_dir = Some(args[idx][2..].to_string()),
            _ if args[idx] == "--mboxrd" => opts.mboxrd = true,
            Some(b'-') if arg.len() == 2 => {
                idx += 1; // `--` marks the end of options
                break;
            }
            _ => return Err(die(&format!("unknown option: {}", args[idx]))),
        }
        idx += 1;
    }

    // `"-"` names stdin, and is what both stdin-only fallbacks substitute in.
    let (dir, inputs): (String, Vec<&str>) = match out_dir {
        // Backwards compatibility: `<dir>`, or `<mbox> <dir>`.
        None => match args.len() - idx {
            1 => (args[idx].clone(), vec!["-"]),
            2 => (args[idx + 1].clone(), vec![args[idx].as_str()]),
            _ => return Err(usage()),
        },
        // New usage: with no more arguments, parse stdin.
        Some(dir) if idx == args.len() => (dir, vec!["-"]),
        Some(dir) => (dir, args[idx..].iter().map(String::as_str).collect()),
    };

    for arg in inputs {
        // stdin is handled before the `stat`, so `-` is never looked up on disk.
        if arg == "-" {
            let Some(ret) = split_mbox(arg, &dir, allow_bare, nr_prec, nr, &opts)? else {
                eprintln!("error: cannot split patches from stdin");
                return Err(Halt(1));
            };
            num += ret - nr;
            nr = ret;
            continue;
        }

        let meta = match std::fs::metadata(arg) {
            Ok(meta) => meta,
            Err(e) => {
                eprintln!("error: cannot stat {arg}: {}", errno_text(&e));
                return Err(Halt(1));
            }
        };

        let ret = if meta.is_dir() {
            split_maildir(arg, &dir, nr_prec, nr, &opts)?
        } else {
            split_mbox(arg, &dir, allow_bare, nr_prec, nr, &opts)?
        };

        let Some(ret) = ret else {
            eprintln!("error: cannot split patches from {arg}");
            return Err(Halt(1));
        };
        num += ret - nr;
        nr = ret;
    }

    println!("{num}");
    Ok(ExitCode::SUCCESS)
}

/// `usage()` — the usage line on stderr, exit 129.
fn usage() -> Halt {
    eprintln!("usage: {USAGE}");
    Halt(129)
}

/// `die()` — `fatal: <msg>` on stderr, exit 128.
fn die(msg: &str) -> Halt {
    eprintln!("fatal: {msg}");
    Halt(128)
}

/// `die_errno()` — `fatal: <msg>: <strerror>` on stderr, exit 128.
fn die_errno(msg: &str, e: &std::io::Error) -> Halt {
    die(&format!("{msg}: {}", errno_text(e)))
}

/// The bare `strerror(errno)` text git prints.
///
/// `std` renders an OS error as `"<strerror> (os error <n>)"`, where the detail
/// half comes from `strerror_r`; removing the suffix recovers git's wording.
fn errno_text(e: &std::io::Error) -> String {
    let text = e.to_string();
    match e.raw_os_error() {
        Some(code) => text
            .strip_suffix(&format!(" (os error {code})"))
            .unwrap_or(&text)
            .to_string(),
        None => text,
    }
}

/// C's `isspace()` in the C locale.
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `strtol(s, NULL, 10)`: optional whitespace, optional sign, then as many
/// decimal digits as follow. No digits yields 0; overflow saturates.
fn strtol(s: &[u8]) -> i64 {
    let mut i = 0;
    while i < s.len() && is_space(s[i]) {
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
    let mut n: i64 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        n = n.saturating_mul(10).saturating_add(i64::from(s[i] - b'0'));
        i += 1;
    }
    if negative {
        -n
    } else {
        n
    }
}

/// `is_from_line()` — does `line` (newline included) look like an mbox `From `
/// envelope header?
///
/// The C walks a cursor backwards from `len - 2`, testing the byte *before* the
/// cursor each step, so the bytes actually examined run from `len - 3` down to
/// index 4. Once a colon is found at index `c` we know `4 <= c <= len - 3`,
/// which is exactly what makes the C's `colon[-4]` and `colon[2]` in-bounds; the
/// same bound makes the indexing below panic-free.
fn is_from_line(line: &[u8]) -> bool {
    let len = line.len();
    if len < 20 || !line.starts_with(b"From ") {
        return false;
    }

    let mut cursor = len - 2;
    let colon = loop {
        if cursor < 5 {
            return false;
        }
        cursor -= 1;
        if line[cursor] == b':' {
            break cursor;
        }
    };

    if !line[colon - 4].is_ascii_digit()
        || !line[colon - 2].is_ascii_digit()
        || !line[colon - 1].is_ascii_digit()
        || !line[colon + 1].is_ascii_digit()
        || !line[colon + 2].is_ascii_digit()
    {
        return false;
    }

    // year
    strtol(&line[colon + 3..]) > 90
}

/// `is_gtfrom()` — an mboxrd-escaped `>{1,}From ` line.
fn is_gtfrom(buf: &[u8]) -> bool {
    if buf.len() < b">From ".len() {
        return false;
    }
    let ngt = buf.iter().take_while(|&&b| b == b'>').count();
    ngt > 0 && buf[ngt..].starts_with(b"From ")
}

/// `strbuf_getwholeline()` — read through the next `\n` inclusive, replacing
/// `buf`. `Ok(false)` means end of input, with `buf` left empty.
fn get_whole_line(input: &mut dyn BufRead, buf: &mut Vec<u8>) -> std::io::Result<bool> {
    buf.clear();
    Ok(input.read_until(b'\n', buf)? != 0)
}

/// `split_one()` — write one message, beginning from the line already in `buf`.
///
/// Copies lines out until a `From ` envelope line opens the next message, or the
/// input runs out; `true` is returned in the latter case (the C `status`).
fn split_one(
    input: &mut dyn BufRead,
    buf: &mut Vec<u8>,
    name: &str,
    allow_bare: bool,
    opts: &Opts,
) -> R<bool> {
    let is_bare = !is_from_line(buf);
    if is_bare && !allow_bare {
        eprintln!("corrupt mailbox");
        return Err(Halt(1));
    }

    // O_WRONLY | O_CREAT | O_EXCL, so an existing target is a hard failure.
    let file = File::options()
        .write(true)
        .create_new(true)
        .open(name)
        .map_err(|e| die_errno(&format!("unable to create '{name}'"), &e))?;
    let mut output = std::io::BufWriter::new(file);

    let status = loop {
        if !opts.keep_cr && buf.len() > 1 && buf.ends_with(b"\r\n") {
            let len = buf.len();
            buf.truncate(len - 2);
            buf.push(b'\n');
        }
        if opts.mboxrd && is_gtfrom(buf) {
            buf.remove(0);
        }
        output
            .write_all(buf)
            .map_err(|e| die_errno("cannot write output", &e))?;

        match get_whole_line(input, buf) {
            Ok(true) => {}
            Ok(false) => break true,
            Err(e) => return Err(die_errno("cannot read mbox", &e)),
        }
        if !is_bare && is_from_line(buf) {
            break false; // done with one message
        }
    };

    // stdio would surface a deferred write error here, where git ignores the
    // `fclose`; reporting it is the same diagnostic it would have produced from
    // the `fwrite` that filled the buffer.
    output
        .flush()
        .map_err(|e| die_errno("cannot write output", &e))?;
    Ok(status)
}

/// The output path for message number `n`: C's `xstrfmt("%s/%0*d", dir, prec, n)`.
fn message_path(dir: &str, nr_prec: i32, n: i32) -> String {
    let width = nr_prec.max(0) as usize;
    format!("{dir}/{n:0width$}")
}

/// `split_mbox()` — split one mbox file; `file` of `"-"` means stdin.
///
/// Returns the new message counter, or `None` for the C `ret < 0` failure that
/// the caller turns into `cannot split patches from ...`.
fn split_mbox(
    file: &str,
    dir: &str,
    allow_bare: bool,
    nr_prec: i32,
    skip: i32,
    opts: &Opts,
) -> R<Option<i32>> {
    let is_stdin = file == "-";
    let mut input: Box<dyn BufRead> = if is_stdin {
        if std::io::stdin().is_terminal() {
            eprintln!("warning: reading patches from stdin/tty...");
        }
        Box::new(std::io::stdin().lock())
    } else {
        match File::open(file) {
            Ok(f) => {
                if f.is_terminal() {
                    eprintln!("warning: reading patches from stdin/tty...");
                }
                Box::new(BufReader::new(f))
            }
            Err(e) => {
                eprintln!("error: cannot open mbox {file}: {}", errno_text(&e));
                return Ok(None);
            }
        }
    };

    // Skip leading whitespace; those bytes are consumed and never written. The
    // first non-space byte is pushed back and opens the first line.
    let first = loop {
        match read_byte(&mut input)? {
            None => {
                if is_stdin {
                    return Ok(Some(skip)); // empty stdin is OK
                }
                eprintln!("error: empty mbox: '{file}'");
                return Ok(None);
            }
            Some(b) if !is_space(b) => break b,
            Some(_) => {}
        }
    };

    let mut buf = vec![first];
    let mut rest = Vec::new();
    get_whole_line(&mut input, &mut rest).map_err(|e| die_errno("cannot read mbox", &e))?;
    buf.extend_from_slice(&rest);

    let mut skip = skip;
    let mut file_done = false;
    while !file_done {
        skip += 1;
        let name = message_path(dir, nr_prec, skip);
        file_done = split_one(&mut input, &mut buf, &name, allow_bare, opts)?;
    }
    Ok(Some(skip))
}

/// One byte from `input`, or `None` at end of input — C's `fgetc()`.
fn read_byte(input: &mut dyn BufRead) -> R<Option<u8>> {
    let mut b = [0u8; 1];
    loop {
        match input.read(&mut b) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(b[0])),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(die_errno("cannot read mbox", &e)),
        }
    }
}

/// `maildir_filename_cmp()` — plain byte comparison, except that digit runs
/// present on both sides compare as integers, so `2` sorts before `10`.
fn maildir_filename_cmp(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (na, next_i) = scan_number(a, i);
            let (nb, next_j) = scan_number(b, j);
            if na != nb {
                return na.cmp(&nb);
            }
            i = next_i;
            j = next_j;
        } else {
            if a[i] != b[j] {
                return a[i].cmp(&b[j]);
            }
            i += 1;
            j += 1;
        }
    }
    // Past the end the C reads the strbuf's NUL terminator, i.e. a zero byte.
    let end_a = a.get(i).copied().unwrap_or(0);
    let end_b = b.get(j).copied().unwrap_or(0);
    end_a.cmp(&end_b)
}

/// `strtol()` over the digit run starting at `at`, plus the index just past it.
fn scan_number(s: &[u8], at: usize) -> (i64, usize) {
    let mut end = at;
    while end < s.len() && s[end].is_ascii_digit() {
        end += 1;
    }
    (strtol(&s[at..end]), end)
}

/// One Maildir message: the `<sub>/<name>` sort key that `string_list_insert`
/// orders on, plus the pieces needed to re-open it without a lossy conversion.
struct MaildirEntry {
    key: Vec<u8>,
    sub: &'static str,
    name: OsString,
}

/// `populate_maildir_list()` driving `string_list_insert()`: scan `cur` then
/// `new`, skip dotfiles, keep the list ordered by [`maildir_filename_cmp`], and
/// drop any entry that compares equal to one already present.
fn populate_maildir_list(path: &str) -> Option<Vec<MaildirEntry>> {
    let mut list: Vec<MaildirEntry> = Vec::new();

    for sub in ["cur", "new"] {
        let shown = format!("{path}/{sub}");
        let dir = match std::fs::read_dir(&shown) {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                eprintln!("error: cannot opendir {shown}: {}", errno_text(&e));
                return None;
            }
        };

        for entry in dir {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    eprintln!("error: cannot opendir {shown}: {}", errno_text(&e));
                    return None;
                }
            };
            let name = entry.file_name();
            if name.as_encoded_bytes().first() == Some(&b'.') {
                continue;
            }

            let mut key = sub.as_bytes().to_vec();
            key.push(b'/');
            key.extend_from_slice(name.as_encoded_bytes());

            // `Err(at)` is "not already present", i.e. the C insert position.
            if let Err(at) = list.binary_search_by(|e| maildir_filename_cmp(&e.key, &key)) {
                list.insert(at, MaildirEntry { key, sub, name });
            }
        }
    }
    Some(list)
}

/// `split_maildir()` — write one output message per file under `cur` and `new`.
///
/// Each mail is split with `allow_bare` set and only one output name is drawn, so
/// a Maildir message that happens to open with a `From ` line is truncated at its
/// second one, exactly as in git.
fn split_maildir(maildir: &str, dir: &str, nr_prec: i32, skip: i32, opts: &Opts) -> R<Option<i32>> {
    let Some(list) = populate_maildir_list(maildir) else {
        return Ok(None);
    };

    let mut skip = skip;
    for entry in &list {
        let path = Path::new(maildir).join(entry.sub).join(&entry.name);
        let shown = format!("{maildir}/{}", String::from_utf8_lossy(&entry.key));

        let file = match File::open(&path) {
            Ok(file) => file,
            Err(e) => {
                eprintln!("error: cannot open mail {shown}: {}", errno_text(&e));
                return Ok(None);
            }
        };
        let mut input = BufReader::new(file);

        let mut buf = Vec::new();
        match get_whole_line(&mut input, &mut buf) {
            Ok(true) => {}
            // An empty mail is C's `strbuf_getwholeline` EOF, reported through
            // `error_errno` with errno untouched by the read — so, errno 0.
            Ok(false) => {
                let e = std::io::Error::from_raw_os_error(0);
                eprintln!("error: cannot read mail {shown}: {}", errno_text(&e));
                return Ok(None);
            }
            Err(e) => {
                eprintln!("error: cannot read mail {shown}: {}", errno_text(&e));
                return Ok(None);
            }
        }

        skip += 1;
        let name = message_path(dir, nr_prec, skip);
        split_one(&mut input, &mut buf, &name, true, opts)?;
    }
    Ok(Some(skip))
}
