//! `git unpack-objects` — read a pack stream from stdin and explode it into
//! loose objects in the current repository.
//!
//! Stock git streams the pack and writes each object the moment it is decoded
//! (`builtin/unpack-objects.c`). This port takes the equivalent route through
//! the vendored `gix-pack`: the stdin stream is indexed into a throwaway
//! pack+idx pair inside a scratch directory under the git dir, every object is
//! then fully resolved through that index (so `OFS_DELTA` and `REF_DELTA`
//! chains, including thin-pack bases already present in the object database,
//! are reconstructed) and written loose. The scratch directory is removed
//! before returning, so the only lasting change is the set of loose objects —
//! matching stock git's end state.
//!
//! Covered:
//!   * the default form: `git unpack-objects < pack`, exit 0, empty stdout.
//!   * `-n` — dry run: the pack is still fully decoded and verified, nothing is
//!     written.
//!   * `-q` — accepted; this port never emits progress, so it is already quiet
//!     (stock git only paints progress when stderr is a terminal).
//!   * `--max-input-size=<n>` — `<n>` is parsed exactly as git's `strtoumax`
//!     does (leading base-10 digits only, so `1k` means 1 and `abc` means 0),
//!     and `0` means "no limit". Over the limit dies with git's message and 128.
//!   * objects already present in the repository are not written again, as git
//!     documents and does.
//!   * `-h` prints the usage line on **stdout** with 129 (git.c intercepts it);
//!     any other unknown flag or any positional argument prints the same line
//!     on **stderr** with 129.
//!   * not being inside a repository: git's `fatal:` line and 128.
//!
//! Not covered — each fails loudly rather than silently diverging:
//!   * `-r` (recover from a corrupt pack): needs git's per-entry salvage loop.
//!     `gix_pack`'s `Mode::Restore` truncates at the first bad entry instead of
//!     skipping it, which recovers a different object set than git does.
//!   * `--strict` / `--strict=<msg-id>`: git runs every unpacked object through
//!     `fsck` with the full message-id severity table; the vendored `gix-fsck`
//!     only offers connectivity traversal, so there is no substrate for it.
//!   * `--pack_header=<version>,<objects>`: git's internal hand-off from
//!     `receive-pack`, which supplies an already-consumed pack header. The
//!     `gix-pack` entry iterator insists on reading the header itself.
//!
//! Two further deliberate differences, both on failure paths only: this port
//! validates the whole pack before writing anything, so a corrupt or
//! oversized pack leaves the object database untouched where git may already
//! have exploded the objects that preceded the corruption; and the `fatal:`
//! text for a malformed pack is `gix-pack`'s diagnostic rather than git's
//! (`early EOF` and friends). The exit code is 128 either way.

use anyhow::{bail, Result};
use std::io::{self, BufRead, Read};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::objs::Write as _;

/// The usage line stock `git unpack-objects` prints, verbatim.
const USAGE: &str = "usage: git unpack-objects [-n] [-q] [-r] [--strict]";

/// `git unpack-objects` — explode a pack read from stdin into loose objects.
///
/// See the module docs for the supported flag set and the deliberate gaps.
pub fn unpack_objects(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands over the arguments after the subcommand; tolerate a
    // leading `unpack-objects` in case the caller passes argv unsliced. The
    // token is never a legal argument here (git answers any positional with
    // the usage line), so dropping it costs no fidelity.
    let args = match args.split_first() {
        Some((first, rest)) if first == "unpack-objects" => rest,
        _ => args,
    };

    let mut dry_run = false;
    let mut max_input_size: u64 = 0; // git: 0 means "unlimited"

    for a in args {
        let a = a.as_str();
        match a {
            "-n" => dry_run = true,
            // Progress is never painted by this port, so `-q` is already true.
            "-q" => {}
            "-h" => {
                println!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-r" => bail!("unsupported flag \"-r\" (ported: -n, -q, --max-input-size=<n>)"),
            _ if a == "--strict" || a.starts_with("--strict=") => {
                bail!("unsupported flag {a:?} — fsck object checking has no substrate in gix-fsck")
            }
            _ if a.starts_with("--pack_header=") => {
                bail!("unsupported flag {a:?} — the pack header cannot be supplied out of band")
            }
            _ if a.starts_with("--max-input-size=") => {
                max_input_size = parse_magnitude(&a["--max-input-size=".len()..]);
            }
            // Any other flag, and any positional, is a usage error for git.
            _ => {
                eprintln!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        }
    }

    let Ok(repo) = gix::discover(".") else {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    };

    // Scratch space for the intermediate pack+idx. It lives under the git dir
    // so the tempfile rename `gix-pack` performs stays on one filesystem. A dry
    // run never needs it: the writer keeps its temporaries elsewhere and
    // discards them.
    let scratch = if dry_run {
        None
    } else {
        Some(Scratch::new(&repo)?)
    };

    let mut input = Limited {
        inner: io::stdin().lock(),
        limit: max_input_size,
        consumed: 0,
    };

    let options = gix::odb::pack::bundle::write::Options {
        iteration_mode: gix::odb::pack::data::input::Mode::Verify,
        object_hash: repo.object_hash(),
        ..Default::default()
    };
    let should_interrupt = AtomicBool::new(false);
    let mut progress = gix::features::progress::Discard;

    let written = gix::odb::pack::Bundle::write_to_directory(
        &mut input,
        // A dry run still decodes and verifies every entry; it just discards
        // the index and pack instead of keeping them around to read back.
        scratch.as_ref().map(|s| s.path.as_path()),
        &mut progress,
        &should_interrupt,
        // Thin packs reference bases by id that only exist in the odb; letting
        // the writer look them up completes the pack the way git resolves them.
        Some(repo.objects.clone()),
        options,
    );

    // git checks the running byte count as it fills its input buffer, so a pack
    // over the limit dies whether or not it is otherwise well formed. Checking
    // the drained total covers both the error and the success path.
    if max_input_size != 0 && input.consumed > max_input_size {
        eprintln!("fatal: pack exceeds maximum allowed size");
        return Ok(ExitCode::from(128));
    }

    let outcome = match written {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };

    if dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    // `to_bundle` is `None` only when nothing was written to disk, which for a
    // non-dry run means an empty pack — a valid input carrying zero objects.
    let Some(bundle) = outcome.to_bundle() else {
        return Ok(ExitCode::SUCCESS);
    };
    let bundle = match bundle {
        Ok(bundle) => bundle,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    };

    let mut buf = Vec::with_capacity(64 * 1024);
    let mut inflate = gix::zlib::Inflate::default();
    let mut cache = gix::odb::pack::cache::Never;

    for idx in 0..bundle.index.num_objects() {
        let id = bundle.index.oid_at_index(idx).to_owned();
        let object = match bundle.get_object_by_index(idx, &mut buf, &mut inflate, &mut cache) {
            Ok((object, _location)) => object,
            Err(e) => {
                eprintln!("fatal: {e}");
                return Ok(ExitCode::from(128));
            }
        };
        // `Repository::write_buf_with_known_id` skips ids the odb already has,
        // which is exactly git's "objects that already exist are not unpacked".
        if let Err(e) = repo.write_buf_with_known_id(object.kind, object.data, id) {
            eprintln!("fatal: {e}");
            return Ok(ExitCode::from(128));
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// git parses `--max-input-size=` with `strtoumax(arg, NULL, 10)`: it consumes
/// the leading run of base-10 digits and ignores the rest, so `1k` is 1 and a
/// value with no leading digit is 0 (which then means "no limit").
fn parse_magnitude(s: &str) -> u64 {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return 0;
    }
    // Out of range saturates the way `strtoumax` does, at the maximum.
    digits.parse().unwrap_or(u64::MAX)
}

/// A scratch directory under the git dir, removed when this value is dropped so
/// the intermediate pack never survives an early return.
struct Scratch {
    path: std::path::PathBuf,
}

impl Scratch {
    fn new(repo: &gix::Repository) -> Result<Self> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let path = repo
            .git_dir()
            .join(format!("zvcs-unpack-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path)?;
        Ok(Scratch { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Stdin wrapper that counts the pack bytes handed downstream and refuses to
/// serve more once `--max-input-size` has been passed, mirroring the check git
/// performs in its `fill()`.
struct Limited<R> {
    inner: R,
    /// The byte budget; `0` means unlimited, as in git.
    limit: u64,
    /// How many bytes the pack reader has taken so far.
    consumed: u64,
}

impl<R> Limited<R> {
    fn over_budget(&self) -> bool {
        self.limit != 0 && self.consumed > self.limit
    }

    fn check(&self) -> io::Result<()> {
        if self.over_budget() {
            return Err(io::Error::other("pack exceeds maximum allowed size"));
        }
        Ok(())
    }
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        let n = self.inner.read(buf)?;
        self.consumed += n as u64;
        Ok(n)
    }
}

impl<R: BufRead> BufRead for Limited<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.check()?;
        self.inner.fill_buf()
    }

    // Only `consume` advances the count for the buffered path; `read` accounts
    // for its own bytes above, and the two paths never overlap.
    fn consume(&mut self, amt: usize) {
        self.consumed += amt as u64;
        self.inner.consume(amt);
    }
}
