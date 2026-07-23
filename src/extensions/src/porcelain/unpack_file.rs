//! `git unpack-file` — write a blob's contents to a temporary file and print its name.
//!
//! Covered: the whole command. `git unpack-file <blob>` takes exactly one
//! argument (any revision spec git's `get_oid` accepts), requires it to name a
//! blob, creates a `0600` file named `.merge_file_XXXXXX` in git's working
//! directory (the worktree root, or the git dir for a bare repository), writes
//! the blob bytes into it, and prints the file name followed by a newline.
//!
//! Not covered: nothing — `unpack-file` has no options. `-h` (and any wrong
//! argument count) prints git's usage line and exits 129, on stdout for a lone
//! `-h` and on stderr otherwise; an unresolvable name or a non-blob object is a
//! fatal error and exits 128.
//!
//! The six `X` characters are random by construction, so the printed name
//! differs from stock git run-for-run; the format, file mode, location, exit
//! code and file contents are identical.

use anyhow::Result;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::hash::ObjectId;
use gix::objs::Kind;

/// git's usage line, printed verbatim on a usage error.
const USAGE: &str = "usage: git unpack-file <blob>";

/// The `mkstemp(3)` template git uses (`builtin/unpack-file.c`).
const TEMPLATE_PREFIX: &str = ".merge_file_";

/// Number of random characters `mkstemp` substitutes for the `XXXXXX` suffix.
const SUFFIX_LEN: usize = 6;

/// The alphabet `mkstemp(3)` draws the suffix from.
const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// How many names to try before giving up, mirroring libc's `TMP_MAX` retry loop.
const MAX_ATTEMPTS: u32 = 238_328;

/// `git unpack-file <blob>` — create a temporary file holding the blob's contents.
///
/// Exactly one positional argument is accepted. It is resolved like git's
/// `repo_get_oid`, i.e. without peeling: `HEAD` yields a commit id (and then
/// fails as a non-blob), while `HEAD:path` and raw blob ids resolve to blobs.
///
/// Exit codes match stock git: 0 on success, 129 for the usage error (no
/// argument, more than one argument, or `-h`), 128 for a name that does not
/// resolve or an object that is not a blob. A lone `-h` writes the usage line
/// to stdout; the other 129 paths write it to stderr.
pub fn unpack_file(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first() {
        Some(a) if a == "unpack-file" => &args[1..],
        _ => args,
    };

    // git checks `argc != 2 || !strcmp(argv[1], "-h")` before anything else, so
    // even `--foo` is treated as an object name rather than an unknown option.
    //
    // Both arms exit 129 but they use different streams. A lone `-h` is
    // intercepted by git.c as an explicit help request and printed to stdout;
    // every other usage error reaches the builtin's own `usage()`, which writes
    // to stderr. Verified against git 2.55.0:
    //
    //   git unpack-file -h          -> stdout, 129
    //   git unpack-file             -> stderr, 129
    //   git unpack-file -h extra    -> stderr, 129
    //   git unpack-file -- <blob>   -> stderr, 129
    let help_requested = args.len() == 1 && args[0] == "-h";
    if help_requested {
        println!("{USAGE}");
        return Ok(ExitCode::from(129));
    }
    if args.len() != 1 {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(129));
    }
    let spec = args[0].as_str();

    let repo = gix::discover(".")?;

    // git's `get_oid` takes a *full-length* hex string verbatim and never checks
    // that the object exists, so a well-formed but bogus id survives resolution
    // and fails later at the blob read. gix's `rev_parse_single` looks the object
    // up instead, so full-length hex is decoded directly to preserve git's
    // diagnostic. Abbreviated hex still has to resolve, in both implementations.
    // Verified against git 2.55.0:
    //
    //   git unpack-file deadbeef…deadbeef  -> fatal: unable to read blob object deadbeef…
    //   git unpack-file deadbeef           -> fatal: Not a valid object name deadbeef
    //
    // Uppercase input is accepted and echoed back lowercased, hence the fold.
    let full_hex = spec.len() == repo.object_hash().len_in_hex()
        && spec.bytes().all(|b| b.is_ascii_hexdigit());
    let direct = full_hex
        .then(|| ObjectId::from_hex(spec.to_ascii_lowercase().as_bytes()).ok())
        .flatten();

    let oid: ObjectId = match direct {
        Some(oid) => oid,
        None => {
            let Ok(id) = repo.rev_parse_single(spec) else {
                eprintln!("fatal: Not a valid object name {spec}");
                return Ok(ExitCode::from(128));
            };
            id.detach()
        }
    };

    // The object must be a blob, and the diagnostic names the resolved id — not
    // the spec the user typed.
    let object = match repo.find_object(oid) {
        Ok(object) if object.kind == Kind::Blob => object,
        _ => {
            eprintln!("fatal: unable to read blob object {}", oid.to_hex());
            return Ok(ExitCode::from(128));
        }
    };

    // git runs with `RUN_SETUP`, which chdirs to the top of the worktree; a bare
    // repository leaves the process where it is (inside the git dir).
    let dir = repo.workdir().unwrap_or_else(|| repo.git_dir()).to_owned();

    let name = create_temp_file(&dir, &object.data)?;
    println!("{name}");
    Ok(ExitCode::SUCCESS)
}

/// Create `<dir>/.merge_file_XXXXXX` exclusively with mode `0600`, write `data`
/// into it, and return the bare file name as git prints it.
fn create_temp_file(dir: &Path, data: &[u8]) -> Result<String> {
    let mut state = seed();

    for _ in 0..MAX_ATTEMPTS {
        let name = candidate_name(&mut state);
        let path: PathBuf = dir.join(&name);

        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true) // O_CREAT | O_EXCL, exactly like mkstemp
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(data)
                    .map_err(|e| anyhow::anyhow!("unable to write temp-file: {e}"))?;
                return Ok(name);
            }
            // A collision just means another name is needed; anything else is fatal.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => anyhow::bail!("unable to create temp-file in {}: {e}", dir.display()),
        }
    }
    anyhow::bail!("unable to create temp-file in {}: no unused name found", dir.display())
}

/// One `.merge_file_XXXXXX` candidate, advancing `state`.
fn candidate_name(state: &mut u64) -> String {
    let mut name = String::with_capacity(TEMPLATE_PREFIX.len() + SUFFIX_LEN);
    name.push_str(TEMPLATE_PREFIX);
    for _ in 0..SUFFIX_LEN {
        let idx = (next(state) % ALPHABET.len() as u64) as usize;
        name.push(ALPHABET[idx] as char);
    }
    name
}

/// Initial PRNG state, mixed from the clock, the pid, and a stack address (ASLR).
fn seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let pid = u64::from(std::process::id());
    let stack = &nanos as *const u64 as u64;
    nanos ^ (pid << 32) ^ stack
}

/// splitmix64 — a small, well-distributed step function. The names only need to
/// be unpredictable enough to avoid collisions; `O_EXCL` provides the guarantee.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
