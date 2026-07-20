//! `git show-index` — dump a packfile index (`.idx`) read from standard input.
//!
//! Covered: the whole command. `show-index` reads a `.idx` file from stdin and
//! prints one line per object, in the order the index stores them (object-id
//! order in a well-formed file):
//!
//! * index v2 — `<pack-offset> <object-id> (<crc32>)`, the CRC as 8 lowercase
//!   zero-padded hex digits; 64-bit offsets are resolved through the large-offset
//!   table exactly as `builtin/show-index.c` does.
//! * index v1 — `<pack-offset> <object-id>`, streamed entry by entry, so a
//!   truncated file still leaves the successfully read lines on stdout.
//!
//! The only option is `--object-format=<sha1|sha256>` (also accepted as two
//! arguments, and as `--no-object-format` to fall back to the default). It only
//! selects the raw hash width used to walk the index, so both algorithms work
//! here even though this build of the vendored `gix-hash` compiles SHA-1 only.
//! Without it the hash comes from the repository (`gix::discover`), and outside a
//! repository git's `assuming SHA-1` warning is emitted before defaulting to SHA-1.
//!
//! Not covered: nothing. Non-option arguments are ignored, matching git's
//! `parse_options` call, which declares no positionals.
//!
//! Exit codes follow git: 0 on success, 129 for `-h` and for an unknown option,
//! 128 for every fatal (unreadable header, unknown index version, corrupt fan-out
//! table, truncated body, unknown hash algorithm).

use anyhow::Result;
use std::fmt::Write as _;
use std::io::Read as _;
use std::process::ExitCode;

/// git's usage block, printed verbatim for `-h` and on a usage error.
const USAGE: &str = "\
usage: git show-index [--object-format=<hash-algorithm>] < <pack-idx-file>

    --[no-]object-format <hash-algorithm>
                          specify the hash algorithm to use
";

/// The `\xfftOc` magic that marks an index of version 2 or newer.
const IDX_SIGNATURE: u32 = 0xff74_4f63;

/// Number of fan-out buckets at the head of every index, one per first byte.
const FAN_LEN: usize = 256;

/// Raw hash width for SHA-1, in bytes.
const SHA1_RAWSZ: usize = 20;

/// Raw hash width for SHA-256, in bytes.
const SHA256_RAWSZ: usize = 32;

/// Report a git-style fatal and return git's exit code for it.
fn fatal(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// A forward-only cursor over the bytes slurped from stdin.
///
/// Every read mirrors one `fread` in `builtin/show-index.c`: it either yields the
/// full request or fails, so the caller can raise git's exact fatal for a short read.
struct Reader {
    data: Vec<u8>,
    pos: usize,
}

impl Reader {
    /// Consume the next `n` bytes, or `None` when fewer than `n` remain.
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.pos.checked_add(n)?;
        if end > self.data.len() {
            return None;
        }
        let out = &self.data[self.pos..end];
        self.pos = end;
        Some(out)
    }
}

/// Decode a big-endian `u32` from the first four bytes of `b`.
fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Decode a big-endian `u64` from the first eight bytes of `b`.
fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// Render raw hash bytes as git's lowercase hex object id.
fn hex(raw: &[u8]) -> String {
    let mut s = String::with_capacity(raw.len() * 2);
    for b in raw {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Map an `--object-format` name to its raw hash width, as `hash_algo_by_name` does.
///
/// git matches the algorithm names exactly, so `SHA1` and `sha-1` are rejected.
fn rawsz_by_name(name: &str) -> Option<usize> {
    match name {
        "sha1" => Some(SHA1_RAWSZ),
        "sha256" => Some(SHA256_RAWSZ),
        _ => None,
    }
}

pub fn show_index(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand being present at index 0 (dispatch strips it today).
    let argv: &[String] = match args.first() {
        Some(a) if a == "show-index" => &args[1..],
        _ => args,
    };

    // `--object-format` is the only option; anything else positional is ignored,
    // because git's `parse_options` call declares no positional arguments.
    let mut hash_name: Option<String> = None;
    let mut no_more_opts = false;
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        if no_more_opts {
            i += 1;
            continue;
        }
        match a {
            "--" => no_more_opts = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--object-format" => {
                let Some(v) = argv.get(i + 1) else {
                    eprintln!("error: option `object-format' requires a value");
                    eprint!("{USAGE}");
                    return Ok(ExitCode::from(129));
                };
                hash_name = Some(v.clone());
                i += 1;
            }
            "--no-object-format" => hash_name = None,
            _ if a.starts_with("--object-format=") => {
                hash_name = Some(a["--object-format=".len()..].to_string());
            }
            // A lone "-" is not an option; anything else starting with a dash is
            // one we do not know. git names long ones "option" and short ones
            // "switch", reporting only the first unknown short character.
            _ if a.len() > 1 && a.starts_with('-') => {
                if let Some(long) = a.strip_prefix("--") {
                    eprintln!("error: unknown option `{long}'");
                } else {
                    let c = a[1..].chars().next().unwrap_or('-');
                    eprintln!("error: unknown switch `{c}'");
                }
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            _ => {}
        }
        i += 1;
    }

    let hashsz = match hash_name {
        Some(name) => match rawsz_by_name(&name) {
            Some(sz) => sz,
            None => return fatal("Unknown hash algorithm"),
        },
        // No explicit format: take the repository's, or warn and assume SHA-1
        // when we are not inside one, exactly as git's setup does.
        None => match gix::discover(".") {
            Ok(repo) => repo.object_hash().len_in_bytes(),
            Err(_) => {
                eprintln!("warning: assuming SHA-1; use --object-format to override");
                SHA1_RAWSZ
            }
        },
    };

    let mut data = Vec::new();
    std::io::stdin().lock().read_to_end(&mut data)?;
    let mut r = Reader { data, pos: 0 };

    // The first two words are either the v2+ signature and version, or the first
    // two fan-out buckets of a v1 index.
    let Some(head) = r.take(2 * 4) else {
        return fatal("unable to read header");
    };
    let (w0, w1) = (be32(head), be32(&head[4..]));

    let mut fanout = [0u32; FAN_LEN];
    let version = if w0 == IDX_SIGNATURE {
        if w1 != 2 {
            return fatal("unknown index version");
        }
        let Some(table) = r.take(FAN_LEN * 4) else {
            return fatal("unable to read index");
        };
        for (slot, chunk) in fanout.iter_mut().zip(table.chunks_exact(4)) {
            *slot = be32(chunk);
        }
        2u32
    } else {
        fanout[0] = w0;
        fanout[1] = w1;
        let Some(table) = r.take((FAN_LEN - 2) * 4) else {
            return fatal("unable to read index");
        };
        for (slot, chunk) in fanout[2..].iter_mut().zip(table.chunks_exact(4)) {
            *slot = be32(chunk);
        }
        1u32
    };

    // The fan-out is cumulative, so it must never decrease.
    let mut nr: u32 = 0;
    for n in fanout {
        if n < nr {
            return fatal("corrupt index file");
        }
        nr = n;
    }
    let nr = nr as usize;

    let mut out = String::new();
    if version == 1 {
        // v1 interleaves offset and id, and git prints each line as it reads it,
        // so a short read leaves the earlier lines on stdout.
        for idx in 0..nr {
            let Some(entry) = r.take(4 + hashsz) else {
                print!("{out}");
                return fatal(format!("unable to read entry {idx}/{nr}"));
            };
            let offset = be32(entry);
            let _ = writeln!(out, "{offset} {}", hex(&entry[4..]));
        }
    } else {
        // v2 stores ids, CRCs and 32-bit offsets as three parallel tables, then a
        // table of 64-bit offsets for the entries whose 32-bit slot has the high
        // bit set and therefore holds an index into it.
        let mut oids: Vec<String> = Vec::with_capacity(nr);
        for idx in 0..nr {
            let Some(raw) = r.take(hashsz) else {
                return fatal(format!("unable to read sha1 {idx}/{nr}"));
            };
            oids.push(hex(raw));
        }

        let mut crcs: Vec<u32> = Vec::with_capacity(nr);
        for idx in 0..nr {
            let Some(raw) = r.take(4) else {
                return fatal(format!("unable to read crc {idx}/{nr}"));
            };
            crcs.push(be32(raw));
        }

        let mut offsets: Vec<u32> = Vec::with_capacity(nr);
        for idx in 0..nr {
            let Some(raw) = r.take(4) else {
                return fatal(format!("unable to read 32b offset {idx}/{nr}"));
            };
            offsets.push(be32(raw));
        }

        let off64_nr = offsets.iter().filter(|o| *o & 0x8000_0000 != 0).count();
        let mut off64: Vec<u64> = Vec::with_capacity(off64_nr);
        if off64_nr > 0 {
            let Some(raw) = r.take(off64_nr * 8) else {
                return fatal("unable to read 64b offsets");
            };
            off64.extend(raw.chunks_exact(8).map(be64));
        }

        for idx in 0..nr {
            let slot = offsets[idx];
            let offset = if slot & 0x8000_0000 != 0 {
                let large = (slot & 0x7fff_ffff) as usize;
                match off64.get(large) {
                    Some(v) => *v,
                    None => {
                        print!("{out}");
                        return fatal("bad 64b offset");
                    }
                }
            } else {
                u64::from(slot)
            };
            let _ = writeln!(out, "{offset} {} ({:08x})", oids[idx], crcs[idx]);
        }
    }

    print!("{out}");
    Ok(ExitCode::SUCCESS)
}
