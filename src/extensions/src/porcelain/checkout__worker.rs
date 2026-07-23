//! `git checkout--worker` — the parallel-checkout helper process.
//!
//! A partial port of `builtin/checkout--worker.c`. Upstream this is never run
//! by a human: `parallel-checkout.c` spawns N copies of it, streams one
//! pkt-line per index entry to write, and reads one reply packet back per item.
//!
//! ### Covered — verified byte-for-byte against stock git 2.55.0
//!
//! * Command-line surface. `git.c` marks this builtin `RUN_SETUP`, so
//!   repository discovery happens *before* option parsing and any command line
//!   run outside a repository fails with
//!   `fatal: not a git repository (or any of the parent directories): .git`
//!   (128) — even `--bogus`. The one exception is the builtin's own
//!   `argc == 2 && !strcmp(argv[1], "-h")` short-circuit, which renders usage
//!   without touching the repository.
//! * `parse_options` for the single `OPT_STRING(0, "prefix", ...)` with no
//!   `PARSE_OPT_*` flags: `--prefix <s>` / `--prefix=<s>`, `--no-prefix`
//!   (clears it), unique-prefix abbreviation (`--pre`, `--p`), `--` as an
//!   option terminator (so `--prefix --` takes `--` as the *value*), and the
//!   four failure shapes —
//!   ``error: unknown option `NAME'`` + usage (129),
//!   ``error: unknown switch `C'`` + usage (129),
//!   ``error: option `NAME' requires a value`` with **no** usage block (129),
//!   ``error: option `NAME' takes no value`` with **no** usage block (129).
//!   `-h` anywhere renders usage on **stdout** and exits 129; any positional
//!   argument renders the same block on **stderr** and exits 129.
//! * `worker_loop`'s `packet_read(0, ...)` framing and its two terminating
//!   paths. A flush (`0000`), a delim (`0001`), a response-end (`0002`) and an
//!   empty packet (`0004`) all read as length 0 and end the loop, after which
//!   `packet_flush(1)` writes `0000` to stdout and the process exits 0. The
//!   read is *not* `PACKET_READ_GENTLE_ON_EOF`, so EOF — whether before the
//!   4-byte header or part-way through a payload — dies with
//!   `fatal: the remote end hung up unexpectedly` (128).
//! * `packet_length` diagnostics: a non-hex header is
//!   `fatal: protocol error: bad line length character: <4 raw bytes>` (128);
//!   a header of `0003` is `fatal: protocol error: bad line length 3` (128);
//!   a header whose payload would not fit `packet_buffer` (`LARGE_PACKET_MAX`,
//!   65520) reports the *payload* length, e.g. `ffff` →
//!   `fatal: protocol error: bad line length 65531` (128).
//!
//! ### Not covered — and why
//!
//! Item packets themselves, i.e. every invocation that does real work. Two
//! independent blockers, neither of which gitoxide can be made to bridge:
//!
//! * **The wire format is a raw C struct dump, not a serialisation format.**
//!   Each request packet is `memcpy`'d out of `struct pc_item_fixed_portion`
//!   (72 bytes on this platform, per git's own
//!   `checkout worker received too short item (got 1B, exp 72B)`), whose layout
//!   is whatever the compiler chose — host endianness, host `size_t`, host
//!   padding, and an embedded `struct object_id`/`struct conv_attrs`. Each
//!   reply packet is `struct pc_item_result`, which embeds a raw
//!   `struct stat`. Reproducing it means pinning a libc ABI; this crate depends
//!   on `gix` and `anyhow` only.
//! * **Writing an entry needs `entry.c` + `convert.c`.** `write_pc_item()`
//!   applies the CRLF action, `ident`, `working-tree-encoding` and smudge
//!   filters that the parent process already resolved into `conv_attrs`, then
//!   `stat()`s the result to fill the reply. gitoxide has the pieces
//!   (`gix-filter`, `gix-worktree-state`) but not behind an interface that
//!   accepts a pre-resolved C `conv_attrs` or returns a `struct stat`.
//!
//! So an item packet `bail!`s rather than guessing. Upstream's two `BUG()`
//! paths for malformed items (`received too short item`, `received corrupted
//! item`) are likewise not reproduced: they abort on `SIGABRT` (134) after
//! printing a `builtin/checkout--worker.c:<line>` prefix that moves with every
//! git release.

use anyhow::{bail, Result};
use std::io::{self, Read, Write};
use std::process::ExitCode;

/// The block `usage_with_options` renders for this command, trailing blank line
/// included. 133 bytes, matching `git checkout--worker -h` exactly.
const USAGE: &str = "usage: git checkout--worker [<options>]\n\n    \
                     --[no-]prefix <string>\n                          \
                     when creating files, prepend <string>\n\n";

/// `packet_buffer`'s size in `builtin/checkout--worker.c`; a payload length at
/// or above it is rejected by `packet_read_with_status`.
const LARGE_PACKET_MAX: usize = 65520;

pub fn checkout__worker(args: &[String]) -> Result<ExitCode> {
    // `args[0]` is the subcommand itself when dispatched; tolerate its absence.
    let rest = match args.first() {
        Some(a) if a == "checkout--worker" => &args[1..],
        _ => args,
    };

    // `git.c` skips `setup_git_directory()` when the whole command line is a
    // lone `-h`, so this must come before repository discovery.
    if rest.len() == 1 && rest[0] == "-h" {
        return Ok(render_help());
    }

    // RUN_SETUP: discovery precedes option parsing, so even a bogus flag
    // outside a repository reports the missing repository first.
    if gix::discover(".").is_err() {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    }

    // `state.base_dir`, used only when writing entries — parsed for fidelity of
    // the diagnostics, then unused by the paths this port covers.
    let _prefix = match parse_options(rest) {
        Ok(prefix) => prefix,
        Err(ParseFailure::Help) => return Ok(render_help()),
        Err(ParseFailure::Message(msg)) => {
            eprint!("{msg}");
            return Ok(ExitCode::from(129));
        }
        Err(ParseFailure::Usage) => {
            eprint!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
    };

    worker_loop()
}

/// `-h`: usage goes to stdout, but the exit code is still 129.
fn render_help() -> ExitCode {
    print!("{USAGE}");
    let _ = io::stdout().flush();
    ExitCode::from(129)
}

// ---------------------------------------------------------------------------
// option parsing
// ---------------------------------------------------------------------------

/// Why `parse_options` stopped short of returning a command line.
enum ParseFailure {
    /// `-h`: render usage on stdout.
    Help,
    /// Text to write verbatim to stderr before exiting 129.
    Message(String),
    /// Render usage on stderr before exiting 129 (a stray positional).
    Usage,
}

/// Mirror `parse_options()` for this command's single `OPT_STRING("prefix")`.
///
/// No `PARSE_OPT_*` flags are set, so `--` terminates options, long options may
/// be abbreviated to any unique prefix, and a separated value is taken from the
/// next argument unconditionally — `--prefix --` yields the literal `--`.
///
/// Returns the final `--prefix` value (`None` after `--no-prefix`).
fn parse_options(args: &[String]) -> Result<Option<String>, ParseFailure> {
    let mut prefix: Option<String> = None;
    let mut no_more_opts = false;
    let mut positionals = 0usize;
    let mut iter = args.iter();

    while let Some(a) = iter.next() {
        if no_more_opts {
            positionals += 1;
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            // Split `--name=value` so the diagnostics name the option exactly
            // as git does, negation included.
            let (name, value) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            let negated = name.strip_prefix("no-");
            let bare = negated.unwrap_or(name);
            if !bare.is_empty() && "prefix".starts_with(bare) {
                if negated.is_some() {
                    if value.is_some() {
                        return Err(ParseFailure::Message(format!(
                            "error: option `{name}' takes no value\n"
                        )));
                    }
                    prefix = None;
                    continue;
                }
                prefix = match value {
                    Some(v) => Some(v.to_string()),
                    None => match iter.next() {
                        Some(v) => Some(v.clone()),
                        None => {
                            return Err(ParseFailure::Message(format!(
                                "error: option `{name}' requires a value\n"
                            )))
                        }
                    },
                };
                continue;
            }
            return Err(ParseFailure::Message(format!(
                "error: unknown option `{long}'\n{USAGE}"
            )));
        }
        // A bare `-` is a positional for git, not a switch. `h` is the only
        // short option this command has, so the first character decides.
        if a.len() > 1 && a.starts_with('-') {
            let c = a[1..].chars().next().expect("length checked above");
            if c == 'h' {
                return Err(ParseFailure::Help);
            }
            return Err(ParseFailure::Message(format!(
                "error: unknown switch `{c}'\n{USAGE}"
            )));
        }
        positionals += 1;
    }

    // Options are scanned first and their diagnostics win; only once the whole
    // command line parsed does `if (argc > 0) usage_with_options(...)` fire.
    if positionals > 0 {
        return Err(ParseFailure::Usage);
    }

    Ok(prefix)
}

// ---------------------------------------------------------------------------
// worker loop
// ---------------------------------------------------------------------------

/// What one `packet_read(0, ...)` produced.
enum Pkt {
    /// A non-empty payload — an encoded `pc_item_fixed_portion` plus variant.
    Item(usize),
    /// Flush, delim, response-end or an empty packet: all read back as length 0
    /// and end the loop.
    Zero,
}

/// `worker_loop()`: drain request packets, then `packet_flush(1)`.
fn worker_loop() -> Result<ExitCode> {
    let stdin = io::stdin();
    let mut input = stdin.lock();

    loop {
        match read_packet(&mut input) {
            Ok(Pkt::Zero) => break,
            Ok(Pkt::Item(len)) => bail!(
                "unsupported: parallel-checkout item packet ({len}B) — its wire format is a raw \
                 C `pc_item_fixed_portion` dump and the reply embeds a raw `struct stat`, and \
                 writing the entry needs convert.c's pre-resolved `conv_attrs` filters (ported: \
                 option parsing, packet framing, the flush/EOF loop exits)"
            ),
            Err(code) => return Ok(code),
        }
    }

    // packet_flush(1)
    let stdout = io::stdout();
    let mut out = stdout.lock();
    out.write_all(b"0000")?;
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// `packet_read(0, packet_buffer, sizeof(packet_buffer), 0)`.
///
/// Reads are not `PACKET_READ_GENTLE_ON_EOF`, so every short read dies. The
/// error variant carries the exit code after the diagnostic has been printed.
fn read_packet(src: &mut impl Read) -> Result<Pkt, ExitCode> {
    let mut head = [0u8; 4];
    if fill(src, &mut head) != 4 {
        return Err(hung_up());
    }

    let mut len = 0usize;
    for b in head {
        let Some(v) = (b as char).to_digit(16) else {
            // `%.4s` of the raw header — write the bytes as they arrived.
            let mut err = io::stderr().lock();
            let _ = err.write_all(b"fatal: protocol error: bad line length character: ");
            let _ = err.write_all(&head);
            let _ = err.write_all(b"\n");
            let _ = err.flush();
            return Err(ExitCode::from(128));
        };
        len = len * 16 + v as usize;
    }

    // 0 = flush, 1 = delim, 2 = response-end; `packet_read` maps all three (and
    // an empty payload) to a length of 0.
    if len < 3 {
        return Ok(Pkt::Zero);
    }
    if len < 4 {
        eprintln!("fatal: protocol error: bad line length {len}");
        return Err(ExitCode::from(128));
    }

    let len = len - 4;
    if len >= LARGE_PACKET_MAX {
        eprintln!("fatal: protocol error: bad line length {len}");
        return Err(ExitCode::from(128));
    }
    if len == 0 {
        return Ok(Pkt::Zero);
    }

    let mut body = vec![0u8; len];
    if fill(src, &mut body) != len {
        return Err(hung_up());
    }
    Ok(Pkt::Item(len))
}

/// `die(_("the remote end hung up unexpectedly"))`.
fn hung_up() -> ExitCode {
    eprintln!("fatal: the remote end hung up unexpectedly");
    ExitCode::from(128)
}

/// Read until `buf` is full or input ends; returns how many bytes were read.
/// An I/O error is indistinguishable from EOF here, which matches git treating
/// a failed `read_in_full` on stdin as the same fatal hang-up.
fn fill(src: &mut impl Read, buf: &mut [u8]) -> usize {
    let mut got = 0;
    while got < buf.len() {
        match src.read(&mut buf[got..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => got += n,
        }
    }
    got
}
