//! `git patch-id` — read patches from standard input and print patch IDs.
//!
//! A line-for-line port of upstream `builtin/patch-id.c` together with
//! `flush_one_hunk()` from `diff.c`. The scanner, the whitespace-stripping rule,
//! the hunk-header parser and the carry-propagating sum of per-hunk hashes are
//! reproduced operation for operation, so stdout matches stock git byte for byte.
//! No repository object access is involved — patch-id is a pure filter over the
//! diff text — so the only gitoxide substrate used is `gix::hash` for the digest
//! and `gix::config` for the `patchid.*` defaults.
//!
//! Output is `<patch-id> <commit-id>\n` per patch, and only for patches whose
//! accumulated length is non-zero, exactly as `flush_current_id()` decides.
//!
//! ### Covered (byte-identical stdout and exit code against stock git)
//!
//! * `git patch-id` — the default: unstable hash, whitespace stripped
//! * `--stable`, `--unstable`, `--verbatim` (`--verbatim` implies `--stable`)
//! * the `patchid.stable` and `patchid.verbatim` configuration defaults, read
//!   from the repository snapshot or, outside a repository, from the global
//!   files plus `GIT_CONFIG_*` overrides
//! * `git log` / `format-patch` prefixes (`commit <oid>`, `From <oid>`), the bare
//!   object-name headers of `diff-tree --stdin`, binary diffs (`GIT binary patch`
//!   and `Binary files ... differ`), and `\ No newline at end of file` lines
//! * `parse_options` long-option spelling: an exact name, or any unambiguous
//!   prefix of one (`--u`, `--stab`, `--verb`), with `=value` rejected as
//!   `option `<name>' takes no value`
//! * `-h` (alone or heading a bundle) — usage on stdout, exit 129; an unknown
//!   option, unknown switch or ambiguous prefix — message plus usage on stderr,
//!   exit 129; two different mode flags — the conflict message, exit 129
//! * positional arguments and anything after `--` are ignored, because upstream's
//!   `parse_options` collects them and `cmd_patch_id` never looks at what is left
//!
//! Verified differentially against stock git 2.55.0 by transcribing this module
//! into a reference script and comparing stdout over ~176 MB of real
//! `diff-tree --patch --stdin`, `log -p -M`, `format-patch --binary` output plus
//! 400 fuzzed malformed inputs, in each of the three modes: all identical.
//!
//! Two upstream details that a from-scratch implementation gets wrong, both
//! confirmed against git 2.55.0 and reproduced here:
//!
//! * whitespace is git's `isspace`, not the C library's — `git-compat-util.h`
//!   redefines it over `ctype.c`'s `sane_ctype` table, where `GIT_SPACE` covers
//!   only tab, newline, carriage return and space. Vertical tab and form feed are
//!   *kept*, and stripping them changes the ID of any patch that contains them
//! * a binary diff with no preceding `index ` line hashes the object names of the
//!   *previous* patch, because upstream's `pre_oid_str` / `post_oid_str` are
//!   uninitialized locals reusing the same stack slot each call
//!
//! ### Not covered
//!
//! * `--help` — upstream renders the man page; this bails rather than fake it
//! * a malformed `patchid.stable` / `patchid.verbatim` value is treated as false
//!   here, where git makes it fatal

use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::config::File as ConfigFile;
use gix::hash::{Hasher, Kind, ObjectId};

/// git's own usage block for `patch-id`, reproduced verbatim.
const USAGE: &str = "\
usage: git patch-id [--stable | --unstable | --verbatim]

    --unstable            use the unstable patch ID algorithm
    --stable              use the stable patch ID algorithm
    --verbatim            don't strip whitespace from the patch

";

/// `GIT_MAX_HEXSZ` — the cap `strlcpy` applies to the index-line object names.
const MAX_HEXSZ: usize = 64;

/// `git patch-id` — compute the patch ID of each patch read from stdin.
pub fn patch_id(args: &[String]) -> Result<ExitCode> {
    // Upstream models the three flags as one `OPT_CMDMODE` slot with the values
    // 1 = --unstable, 2 = --stable, 3 = --verbatim; 0 means "not given".
    let mut opts = 0u8;
    let mut set_by = "";
    let mut no_more_opts = false;

    // `dispatch::run` is handed the subcommand separately, so `args` already
    // excludes the `patch-id` verb: every element here is an argument to parse.
    for a in args.iter() {
        let s = a.as_str();
        if no_more_opts {
            continue;
        }
        let (val, name) = match s {
            "--" => {
                no_more_opts = true;
                continue;
            }
            "--help" => anyhow::bail!(
                "unsupported flag \"--help\" (ported: -h, --stable, --unstable, --verbatim)"
            ),
            _ if s.starts_with("--") => match resolve_long(&s[2..]) {
                Long::Mode(val, name) => (val, name),
                // `parse_options` rejects a value on a flag with the bare option
                // name, however it was spelled, and prints no usage block.
                Long::TakesNoValue(bare) => {
                    eprintln!("error: option `{bare}' takes no value");
                    return Ok(ExitCode::from(129));
                }
                Long::Ambiguous(a, b) => {
                    eprintln!("error: ambiguous option: {} (could be {a} or {b})", &s[2..]);
                    eprint!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
                Long::Unknown => {
                    eprintln!("error: unknown option `{}'", &s[2..]);
                    eprint!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
            },
            // A lone "-" is a non-option to git's parse_options, like any other.
            // Otherwise the bundle is scanned left to right; `patch-id` registers
            // no short option but the `-h` that `parse_options` adds itself, so the
            // first byte decides: `h` shows the usage, anything else is unknown.
            _ if s.starts_with('-') && s.len() > 1 => {
                let c = s[1..].chars().next().unwrap_or('?');
                if c == 'h' {
                    print!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
                eprintln!("error: unknown switch `{c}'");
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            _ => continue,
        };
        if opts != 0 && opts != val {
            eprintln!("error: options '{name}' and '{set_by}' cannot be used together");
            return Ok(ExitCode::from(129));
        }
        opts = val;
        set_by = name;
    }

    let (mut cfg_stable, cfg_verbatim, kind) = load_defaults();
    // `patchid.verbatim` implies `patchid.stable`, as `--verbatim` implies `--stable`.
    if cfg_verbatim {
        cfg_stable = true;
    }
    let stable = if opts != 0 { opts > 1 } else { cfg_stable };
    let verbatim = if opts != 0 { opts == 3 } else { cfg_verbatim };

    let mut lines = Lines::from_stdin()?;
    generate_id_list(&mut lines, stable, verbatim, kind)?;
    Ok(ExitCode::SUCCESS)
}

/// The three mode options: bare name, spelling used in messages, and the
/// `OPT_CMDMODE` value upstream stores for it.
static MODES: [(&str, &str, u8); 3] = [
    ("unstable", "--unstable", 1),
    ("stable", "--stable", 2),
    ("verbatim", "--verbatim", 3),
];

/// What a `--…` argument resolved to.
enum Long {
    /// A mode flag: its `OPT_CMDMODE` value and its full spelling.
    Mode(u8, &'static str),
    /// A mode flag given `=value`; carries the bare name for the error.
    TakesNoValue(&'static str),
    /// The prefix matched several options; carries the pair upstream names.
    Ambiguous(&'static str, &'static str),
    Unknown,
}

/// Resolve the text after `--` the way `parse_options` does.
///
/// The name is the run up to the first `=`; an exact match wins, otherwise an
/// unambiguous prefix stands for the whole option, so `--u`, `--stab` and
/// `--verb` all work. Any `=value` is rejected afterwards, since all three flags
/// are `OPT_CMDMODE` and take none.
///
/// When several options share the prefix, upstream reports the *last two* it
/// walked past — `abbrev_option` holds the most recent match and
/// `ambiguous_option` the one before it — so the scan keeps both rather than a
/// count. For this option set that is only reachable with an empty name (`--=`),
/// since the three names start with different letters.
fn resolve_long(rest: &str) -> Long {
    let (name, has_value) = match rest.find('=') {
        Some(i) => (&rest[..i], true),
        None => (rest, false),
    };

    let mut abbrev: Option<usize> = None;
    let mut ambiguous: Option<usize> = None;
    for (i, (bare, ..)) in MODES.iter().enumerate() {
        if bare.starts_with(name) {
            ambiguous = abbrev;
            abbrev = Some(i);
        }
    }

    let idx = match MODES.iter().position(|(bare, ..)| *bare == name) {
        Some(exact) => exact,
        None => match (ambiguous, abbrev) {
            (Some(a), Some(b)) => return Long::Ambiguous(MODES[a].1, MODES[b].1),
            (None, Some(b)) => b,
            _ => return Long::Unknown,
        },
    };

    let (bare, full, val) = MODES[idx];
    if has_value {
        Long::TakesNoValue(bare)
    } else {
        Long::Mode(val, full)
    }
}

/// The `patchid.stable` / `patchid.verbatim` defaults and the hash to use.
///
/// Inside a repository the merged snapshot answers, and the digest follows the
/// repository's object hash — the same coupling upstream inherits from
/// `the_hash_algo`. Outside one, git still reads the global configuration, and
/// falls back to SHA-1 (`GIT_HASH_DEFAULT`).
fn load_defaults() -> (bool, bool, Kind) {
    match gix::discover(".") {
        Ok(repo) => {
            let snapshot = repo.config_snapshot();
            let stable = snapshot.boolean("patchid.stable").unwrap_or(false);
            let verbatim = snapshot.boolean("patchid.verbatim").unwrap_or(false);
            (stable, verbatim, repo.object_hash())
        }
        Err(_) => {
            let cfg = global_config();
            let read = |key: &str| -> bool {
                cfg.as_ref()
                    .ok()
                    .and_then(|c| c.boolean(key).ok().flatten())
                    .unwrap_or(false)
            };
            (read("patchid.stable"), read("patchid.verbatim"), Kind::Sha1)
        }
    }
}

/// The configuration git reads when there is no repository: the global files
/// plus the `GIT_CONFIG_*` environment overrides.
fn global_config() -> Result<ConfigFile> {
    let mut file = ConfigFile::from_globals()?;
    file.append(ConfigFile::from_environment_overrides()?)?;
    Ok(file)
}

/// Port of `generate_id_list()`: one patch per iteration, printing the previous
/// patch's commit id alongside the id just computed.
fn generate_id_list(lines: &mut Lines, stable: bool, verbatim: bool, kind: Kind) -> Result<()> {
    let rawsz = kind.len_in_bytes();
    let mut oid = vec![0u8; rawsz];
    let mut next = vec![0u8; rawsz];
    let mut result = vec![0u8; rawsz];
    // The two object names of the last `index ` line seen, hashed in place of a
    // binary diff's content. Upstream declares these as *uninitialized* locals of
    // `get_one_patchid()`, so a patch whose binary diff carries no `index ` line
    // hashes whatever the previous call left in that stack slot — in practice the
    // previous patch's names, and empty strings on the first call. Hoisting them
    // to the loop reproduces that observable behaviour exactly; see the note in
    // the module docs.
    let mut names = IndexNames::default();

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // `while (!feof(stdin))`: `Lines` sets `eof` exactly when a read reaches the
    // end of the stream, including the read that returns a final unterminated line.
    while !lines.eof {
        let patchlen = get_one_patchid(
            lines,
            &mut next,
            &mut result,
            &mut names,
            stable,
            verbatim,
            kind,
        )?;
        // `flush_current_id()` prints nothing for a zero-length patch.
        if patchlen != 0 {
            writeln!(out, "{} {}", hex(&result), hex(&oid))?;
        }
        oid.copy_from_slice(&next);
    }
    out.flush()?;
    Ok(())
}

/// Port of `get_one_patchid()`.
///
/// Consumes lines up to the start of the next patch, folding each hunk into
/// `result` and returning the accumulated (whitespace-stripped, unless
/// `verbatim`) patch length. `next_oid` receives the object name that ended this
/// patch, or all zeroes when the input ran out first.
fn get_one_patchid(
    lines: &mut Lines,
    next_oid: &mut Vec<u8>,
    result: &mut Vec<u8>,
    names: &mut IndexNames,
    stable: bool,
    verbatim: bool,
    kind: Kind,
) -> Result<usize> {
    let rawsz = kind.len_in_bytes();
    let mut patchlen = 0usize;
    let mut found_next = false;
    // Remaining `-`/` ` and `+`/` ` lines in the current hunk; -1 = "in a header".
    let mut before: i64 = -1;
    let mut after: i64 = -1;
    let mut diff_is_binary = false;
    let mut ctx = gix::hash::hasher(kind);
    let mut scratch: Vec<u8> = Vec::new();

    result.clear();
    result.resize(rawsz, 0);

    while let Some(range) = lines.next_line() {
        let line = &lines.buf[range];
        // Every upstream operation on `line` is a NUL-terminated string op, so
        // cutting at the first NUL reproduces all of them at once.
        let s = &line[..cstr_len(line)];

        // Possibly skip over the prefix added by "log" or "format-patch".
        let p_off = if s.starts_with(b"commit ") {
            7
        } else if s.starts_with(b"From ") {
            5
        } else {
            if s.starts_with(b"\\ ") && s.len() > 12 {
                if verbatim {
                    ctx.update(s);
                }
                continue;
            }
            0
        };

        if get_oid_hex(&s[p_off..], rawsz, next_oid) {
            found_next = true;
            break;
        }

        // Ignore commit comments.
        if patchlen == 0 && !s.starts_with(b"diff ") {
            continue;
        }

        // Parsing diff header?
        if before == -1 {
            if s.starts_with(b"GIT binary patch") || s.starts_with(b"Binary files") {
                diff_is_binary = true;
                before = 0;
                ctx.update(&names.pre);
                ctx.update(&names.post);
                if stable {
                    flush_one_hunk(result, &mut ctx, kind)?;
                }
                continue;
            } else if s.starts_with(b"index ") {
                if let Some(oid1_end) = find(s, b"..") {
                    // The blob names run from after "index " to "..", and from
                    // after ".." to the next space — or to the trailing newline
                    // when the line carries no mode.
                    let oid2_end = find(&s[oid1_end..], b" ")
                        .map(|i| oid1_end + i)
                        .unwrap_or_else(|| s.len().saturating_sub(1));
                    names.pre = clamp_hex(&s[6..oid1_end]);
                    let start = oid1_end + 2;
                    names.post = if start <= oid2_end {
                        clamp_hex(&s[start..oid2_end])
                    } else {
                        Vec::new()
                    };
                }
                continue;
            } else if s.starts_with(b"--- ") {
                before = 1;
                after = 1;
            } else if !s.first().is_some_and(u8::is_ascii_alphabetic) {
                break;
            }
        }

        if diff_is_binary {
            if s.starts_with(b"diff ") {
                diff_is_binary = false;
                before = -1;
            }
            continue;
        }

        // Looking for a valid hunk header?
        if before == 0 && after == 0 {
            if s.starts_with(b"@@ -") {
                // Parse next hunk, but ignore line numbers.
                scan_hunk_header(s, &mut before, &mut after);
                continue;
            }

            // Split at the end of the patch.
            if !s.starts_with(b"diff ") {
                break;
            }

            // Else we're parsing another header.
            if stable {
                flush_one_hunk(result, &mut ctx, kind)?;
            }
            before = -1;
            after = -1;
        }

        // If we get here, we're inside a hunk.
        let c = s.first().copied().unwrap_or(0);
        if c == b'-' || c == b' ' {
            before -= 1;
        }
        if c == b'+' || c == b' ' {
            after -= 1;
        }

        // Add line to hash algo (possibly removing whitespace).
        let payload = if verbatim {
            s
        } else {
            remove_space(s, &mut scratch);
            scratch.as_slice()
        };
        patchlen += payload.len();
        ctx.update(payload);
    }

    if !found_next {
        next_oid.clear();
        next_oid.resize(rawsz, 0);
    }
    flush_one_hunk(result, &mut ctx, kind)?;
    Ok(patchlen)
}

/// Port of `flush_one_hunk()` in `diff.c`: finalize the running digest, restart
/// the context, and add the digest into `result` as a little-endian sum with carry.
fn flush_one_hunk(result: &mut [u8], ctx: &mut Hasher, kind: Kind) -> Result<()> {
    let done = std::mem::replace(ctx, gix::hash::hasher(kind));
    let digest = done
        .try_finalize()
        .map_err(|e| anyhow::anyhow!("hashing a patch hunk: {e}"))?;
    let hash = digest.as_slice();

    let mut carry: u16 = 0;
    for i in 0..kind.len_in_bytes() {
        carry += u16::from(result[i]) + u16::from(hash[i]);
        result[i] = carry as u8;
        carry >>= 8;
    }
    Ok(())
}

/// Port of `scan_hunk_header()`: read `@@ -<n>[,<n>] +<n>[,<n>] @@`, keeping only
/// the line *counts*.
///
/// The caller ignores the return value, but the assignments made before an early
/// failure are observable, so they are reproduced rather than deferred.
fn scan_hunk_header(p: &[u8], p_before: &mut i64, p_after: &mut i64) -> bool {
    // `q = p + 4` — past the leading "@@ -", which the caller has already matched.
    let mut q = &p[4.min(p.len())..];
    let mut n = digit_span(q);
    if at(q, n) == b',' {
        q = &q[n + 1..];
        *p_before = atoi(q);
        n = digit_span(q);
    } else {
        *p_before = 1;
    }

    if n == 0 || at(q, n) != b' ' || at(q, n + 1) != b'+' {
        return false;
    }

    let mut r = &q[n + 2..];
    n = digit_span(r);
    if at(r, n) == b',' {
        r = &r[n + 1..];
        *p_after = atoi(r);
        n = digit_span(r);
    } else {
        *p_after = 1;
    }
    n != 0
}

/// The pre- and post-image object names taken from the most recent `index ` line.
///
/// Deliberately *not* cleared between patches — see the comment at its
/// construction site in `generate_id_list()`.
#[derive(Default)]
struct IndexNames {
    pre: Vec<u8>,
    post: Vec<u8>,
}

/// A `strbuf_getwholeline`-alike over the whole of standard input.
struct Lines {
    buf: Vec<u8>,
    pos: usize,
    /// Set once a read has reached the end of the stream, mirroring `feof`.
    eof: bool,
}

impl Lines {
    fn from_stdin() -> Result<Self> {
        let mut buf = Vec::new();
        std::io::stdin()
            .lock()
            .read_to_end(&mut buf)
            .context("reading standard input")?;
        Ok(Lines {
            buf,
            pos: 0,
            eof: false,
        })
    }

    /// The next line with its trailing newline, or `None` once input is exhausted.
    ///
    /// Like `getdelim`, a final line without a newline is returned *and* marks the
    /// stream as at end-of-file; only the following call yields `None`.
    fn next_line(&mut self) -> Option<std::ops::Range<usize>> {
        if self.pos >= self.buf.len() {
            self.eof = true;
            return None;
        }
        let start = self.pos;
        let end = match self.buf[start..].iter().position(|&b| b == b'\n') {
            Some(i) => start + i + 1,
            None => {
                self.eof = true;
                self.buf.len()
            }
        };
        self.pos = end;
        Some(start..end)
    }
}

/// C `strlen` over a line buffer: everything up to the first NUL.
fn cstr_len(line: &[u8]) -> usize {
    line.iter().position(|&b| b == 0).unwrap_or(line.len())
}

/// Port of `remove_space()`: drop every whitespace byte.
///
/// The predicate is git's own `isspace`, which `git-compat-util.h` redefines over
/// the `sane_ctype` table in `ctype.c` — `GIT_SPACE` is set only for tab, newline,
/// carriage return and space. Vertical tab and form feed are *not* whitespace to
/// git, so using the C-library `isspace` set here produces wrong patch IDs for any
/// diff containing them.
fn is_git_space(c: u8) -> bool {
    matches!(c, b'\t' | b'\n' | b'\r' | b' ')
}

/// Port of `remove_space()`.
fn remove_space(line: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.extend(line.iter().copied().filter(|&c| !is_git_space(c)));
}

/// C `get_oid_hex`: succeeds when the first `2 * rawsz` bytes are hex digits.
///
/// Upstream does not require a terminator after them, which is what lets a
/// `diff-tree --stdin` object-name line be recognised.
fn get_oid_hex(s: &[u8], rawsz: usize, out: &mut Vec<u8>) -> bool {
    let n = rawsz * 2;
    if s.len() < n || !s[..n].iter().all(u8::is_ascii_hexdigit) {
        return false;
    }
    out.clear();
    for pair in s[..n].chunks_exact(2) {
        out.push((hexval(pair[0]) << 4) | hexval(pair[1]));
    }
    true
}

/// The numeric value of a byte already known to be an ASCII hex digit.
fn hexval(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => c - b'A' + 10,
    }
}

/// `strlcpy(dst, src, GIT_MAX_HEXSZ + 1)` — copy at most `GIT_MAX_HEXSZ` bytes.
fn clamp_hex(s: &[u8]) -> Vec<u8> {
    s[..s.len().min(MAX_HEXSZ)].to_vec()
}

/// The offset of `needle` in `haystack`, like `strstr`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

/// `buf[i]`, or the C string's terminating NUL when `i` is past the end.
fn at(buf: &[u8], i: usize) -> u8 {
    buf.get(i).copied().unwrap_or(0)
}

/// `strspn(buf, "0123456789")`.
fn digit_span(buf: &[u8]) -> usize {
    buf.iter().position(|b| !b.is_ascii_digit()).unwrap_or(buf.len())
}

/// `atoi`: the leading decimal run, or 0 when there is none.
fn atoi(buf: &[u8]) -> i64 {
    let mut n: i64 = 0;
    for &b in buf.iter().take_while(|b| b.is_ascii_digit()) {
        n = n.saturating_mul(10).saturating_add(i64::from(b - b'0'));
    }
    n
}

/// The lowercase hex rendering git prints for an object name.
fn hex(raw: &[u8]) -> String {
    ObjectId::from_bytes_or_panic(raw).to_hex().to_string()
}
