//! `git backfill` — download missing objects in a partial clone.
//!
//! What is covered: everything `backfill` does in a repository that has **no
//! promisor remote**, which is every repository that was not created by
//! `git clone --filter=...`. There, stock git walks the requested revision
//! range, finds nothing it could ask a server for, prints nothing, changes no
//! repository state and exits 0 — verified against git 2.55.0, including the
//! case where objects are genuinely absent from the object database (without a
//! promisor remote there is nowhere to fetch them from, so git still exits 0
//! silently). This module reproduces that exactly, after doing the argument,
//! revision and sparse-checkout validation that runs first, so every observable
//! failure path keeps git's bytes and exit code:
//!
//!   * `-h` — the 340-byte usage block on stdout, exit 129. As in `git.c`, only
//!     the exact invocation `git backfill -h` skips repository setup.
//!   * `--min-batch-size=<n>` / `--min-batch-size <n>` — validated with git's
//!     `git_parse_ulong` semantics (`strtoumax` base 0, optional `k`/`m`/`g`
//!     factor, any `-` rejected outright), producing git's three distinct
//!     parse-options errors on exit 129.
//!   * `--sparse` / `--no-sparse` — when sparse mode is on (explicitly, or
//!     implicitly via `core.sparseCheckout`), a `$GIT_DIR/info/sparse-checkout`
//!     that cannot be read yields `error: problem loading sparse-checkout` and
//!     exit 255, git's `return error(...)` shape.
//!   * `--include-edges` / `--no-include-edges` — accepted; they only steer
//!     which blobs would be downloaded.
//!   * the `<revision-range>`, resolved for real, so `^<bad>` gives
//!     `fatal: bad revision` and a bad rev or range gives the
//!     `fatal: ambiguous argument` block, both on exit 128. An argument that
//!     names an existing path is a pathspec, not an error, as in git.
//!   * an unrecognized argument gives `fatal: unrecognized argument: <arg>`,
//!     exit 128 — git's `setup_revisions` wording, not parse-options'.
//!
//! What is **not** covered: the download itself, i.e. `backfill`'s entire reason
//! for existing in a partial clone. The vendored gitoxide has no partial-clone
//! support at all: no crate mentions promisor remotes or `extensions.partialClone`,
//! `gix-protocol`'s fetch arguments expose no `filter` line (the string appears
//! only in the accepted-capability list in `gix-protocol/src/command.rs:44`), and
//! there is no client path that requests explicit blob ids. So when the
//! repository *does* have a promisor remote, this bails naming that gap rather
//! than exiting 0 and leaving the missing blobs undownloaded — which would be
//! indistinguishable from success while silently failing the command's purpose.
//!
//! Commit-limiting options that `setup_revisions` accepts (`--first-parent`,
//! `--all`, `--since=`, `--merges`, …) are accepted and have no effect here.
//! That is sound only because the ported path is a proven no-op: with no
//! promisor remote the chosen revision set cannot change stdout, the exit code
//! or repository state. Any repository where the revision set *would* matter has
//! a promisor remote, and that case bails before returning success. Options
//! outside the verified accept-list below are rejected exactly as git rejects
//! them, so nothing unknown is ever silently swallowed. `--stdin` is the one
//! form git accepts that this rejects: it feeds revisions in from stdin, and
//! consuming them here without a walk to spend them on would hide invalid input
//! that git reports.

use anyhow::{bail, Result};
use std::path::Path;
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;

/// `git backfill -h`, byte-for-byte (340 bytes, git 2.55.0).
const USAGE: &str = "usage: git backfill [--min-batch-size=<n>] [--[no-]sparse] [--[no-]include-edges] [<revision-range>]\n\
                     \n\
                     \x20   --min-batch-size <n>  Minimum number of objects to request at a time\n\
                     \x20   --[no-]sparse         Restrict the missing objects to the current sparse-checkout\n\
                     \x20   --[no-]include-edges  Include blobs from boundary commits in the backfill\n\
                     \n";

/// Options `setup_revisions` accepts as a bare word, verified one by one against
/// git 2.55.0 (`git backfill <opt>` exits 0). They restrict which commits are
/// walked, which cannot change this port's output — see the module docs.
const REV_OPTS: [&str; 35] = [
    "--first-parent",
    "--all",
    "--not",
    "--reverse",
    "--objects",
    "--tags",
    "--branches",
    "--remotes",
    "--no-walk",
    "--do-walk",
    "--topo-order",
    "--date-order",
    "--author-date-order",
    "--boundary",
    "--merges",
    "--no-merges",
    "--full-history",
    "--simplify-merges",
    "--dense",
    "--no-min-parents",
    "--no-max-parents",
    "--cherry-pick",
    "--left-only",
    "--right-only",
    "--bisect",
    "--walk-reflogs",
    "--children",
    "--parents",
    "--quiet",
    "--in-commit-order",
    "--unpacked",
    "--single-worktree",
    "--reflog",
    "--alternate-refs",
    "--indexed-objects",
];

/// Options `setup_revisions` accepts in `--name=<value>` form, verified the same
/// way. `--ancestry-path` appears here too because git 2.55 takes both the bare
/// and the `=<commit>` spelling.
const REV_OPTS_WITH_VALUE: [&str; 12] = [
    "--since",
    "--after",
    "--until",
    "--before",
    "--max-count",
    "--skip",
    "--min-parents",
    "--max-parents",
    "--glob",
    "--exclude",
    "--filter",
    "--ancestry-path",
];

/// `git backfill [--min-batch-size=<n>] [--[no-]sparse] [--[no-]include-edges] [<revision-range>]`.
///
/// Validates arguments, revisions and sparse-checkout state the way stock git
/// does, then performs the no-op that git performs when the repository has no
/// promisor remote. Bails when one is configured; see the module documentation.
pub fn backfill(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `backfill` is never a revision this
    // command would be asked about, so dropping a leading copy is unambiguous.
    let args = match args.first().map(String::as_str) {
        Some("backfill") => &args[1..],
        _ => args,
    };

    // `git.c` skips repository setup only for the exact invocation `git <cmd> -h`,
    // so this is the one path that works outside a repository.
    if args.len() == 1 && args[0] == "-h" {
        print!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let repo = gix::discover(".")?;

    // Pass one: parse-options over the whole argv, stopping at `--`, which it
    // leaves in place for `setup_revisions`. Its errors precede every revision
    // error, matching git's ordering.
    let mut sparse: Option<bool> = None;
    let mut rest: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--" => {
                rest.extend(args[i..].iter().map(String::as_str));
                break;
            }
            "--sparse" => sparse = Some(true),
            "--no-sparse" => sparse = Some(false),
            "--include-edges" | "--no-include-edges" => {}
            "--min-batch-size" => {
                let Some(value) = args.get(i + 1) else {
                    return Ok(bare_error("option `min-batch-size' requires a value"));
                };
                if let Err(code) = parse_magnitude(value) {
                    return Ok(magnitude_error(value, code));
                }
                i += 1;
            }
            _ if a.starts_with("--min-batch-size=") => {
                let value = &a["--min-batch-size=".len()..];
                if let Err(code) = parse_magnitude(value) {
                    return Ok(magnitude_error(value, code));
                }
            }
            _ => rest.push(a),
        }
        i += 1;
    }

    // Pass two: `setup_revisions` over what parse-options left, in source order.
    let mut negating = false;
    let mut has_bottom = false;
    let mut saw_objects = false;
    let mut saw_filter = false;
    let mut saw_ancestry_path = false;

    let mut j = 0;
    while j < rest.len() {
        let a = rest[j];
        j += 1;

        // Everything after `--` is a pathspec; git does not require it to exist.
        if a == "--" {
            break;
        }

        if let Some(spec) = a.strip_prefix('^') {
            // git reports the caret form differently from a bare revision.
            if resolve(&repo, spec).is_none() {
                eprintln!("fatal: bad revision '{a}'");
                return Ok(ExitCode::from(128));
            }
            has_bottom = true;
            continue;
        }

        if a.len() > 1 && a.starts_with('-') {
            // `-<n>` is rev-list's max-count shorthand.
            if a[1..].bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let name = a.split_once('=').map_or(a, |(name, _)| name);
            if REV_OPTS.contains(&a) || REV_OPTS_WITH_VALUE.contains(&name) {
                match name {
                    "--not" => negating = !negating,
                    "--objects" => saw_objects = true,
                    "--filter" => saw_filter = true,
                    "--ancestry-path" => saw_ancestry_path = true,
                    _ => {}
                }
                continue;
            }
            if a == "--stdin" {
                bail!(
                    "unsupported flag \"--stdin\" (ported: --min-batch-size, --sparse/--no-sparse, \
                     --include-edges/--no-include-edges, <revision-range>): it reads revisions from \
                     stdin, and this port has no walk to spend them on, so invalid input git reports \
                     would go unreported"
                );
            }
            eprintln!("fatal: unrecognized argument: {a}");
            return Ok(ExitCode::from(128));
        }

        // A positional: a revision, a range, or — if it names an existing path —
        // a pathspec, which git's `verify_filename` accepts without complaint.
        let resolved = match a.split_once("..") {
            Some((left, right)) => {
                let right = right.strip_prefix('.').unwrap_or(right); // `<a>...<b>`
                let left = if left.is_empty() { "HEAD" } else { left };
                let right = if right.is_empty() { "HEAD" } else { right };
                resolve(&repo, left).is_some() && resolve(&repo, right).is_some()
            }
            None => resolve(&repo, a).is_some(),
        };
        if resolved {
            has_bottom |= negating || a.contains("..");
            continue;
        }
        if Path::new(a).exists() {
            continue;
        }
        return Ok(fatal_ambiguous(a));
    }

    // Two post-parse checks git makes once the whole revision set is known.
    if saw_filter && !saw_objects {
        eprintln!("fatal: object filtering requires --objects");
        return Ok(ExitCode::from(128));
    }
    if saw_ancestry_path && !has_bottom {
        eprintln!("fatal: --ancestry-path given but there are no bottom commits");
        return Ok(ExitCode::from(128));
    }

    // Sparse mode defaults to whatever `core.sparseCheckout` says. git loads the
    // patterns before doing any work, and an unreadable file is fatal.
    let sparse = sparse.unwrap_or_else(|| {
        repo.config_snapshot()
            .boolean("core.sparseCheckout")
            .unwrap_or(false)
    });
    if sparse {
        let patterns = repo.git_dir().join("info").join("sparse-checkout");
        if std::fs::read(&patterns).is_err() {
            eprintln!("error: problem loading sparse-checkout");
            // git's `return error(...)` propagates -1 out of `run_builtin`.
            return Ok(ExitCode::from(255));
        }
    }

    if has_promisor_remote(&repo) {
        bail!(
            "backfill cannot download from a promisor remote: the vendored gitoxide has no \
             partial-clone support — no crate mentions promisor remotes or extensions.partialClone, \
             gix-protocol's fetch arguments expose no filter line, and there is no way to request \
             explicit blob ids (ported: argument, revision and sparse-checkout validation, and the \
             complete no-op git performs when no promisor remote is configured)"
        );
    }

    // No promisor remote: there is nothing to request and nothing to write.
    // Stock git prints nothing, touches nothing and exits 0.
    Ok(ExitCode::SUCCESS)
}

/// Peel `spec` to a commit id, or `None` when it names no commit.
fn resolve(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let object = repo.rev_parse_single(spec).ok()?.object().ok()?;
    Some(object.peel_to_commit().ok()?.id)
}

/// git's `setup_revisions` failure for an argument that is neither a revision
/// nor an existing path: the fatal block on stderr, exit code 128.
fn fatal_ambiguous(spec: &str) -> ExitCode {
    eprintln!(
        "fatal: ambiguous argument '{spec}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    );
    ExitCode::from(128)
}

/// A parse-options `error:` line with no usage block after it, exit 129.
fn bare_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(129)
}

/// Why `--min-batch-size`'s value was rejected. git prints a different line for
/// each, all on exit 129.
enum MagnitudeError {
    /// The value was empty.
    Empty,
    /// It did not parse as a number with an optional `k`/`m`/`g` factor.
    Malformed,
    /// It parsed but overflowed `uintmax_t` (`errno == ERANGE`).
    Range,
}

/// Render the rejection the way parse-options does.
fn magnitude_error(value: &str, kind: MagnitudeError) -> ExitCode {
    match kind {
        MagnitudeError::Empty => bare_error("option `min-batch-size' expects a numerical value"),
        MagnitudeError::Malformed => bare_error(
            "option `min-batch-size' expects a non-negative integer value with an optional k/m/g suffix",
        ),
        // git prints the unsigned maximum through a signed format, hence `-1`.
        MagnitudeError::Range => bare_error(&format!(
            "value {value} for option `min-batch-size' not in range [0,-1]"
        )),
    }
}

/// Port of git's `git_parse_ulong` as parse-options' `OPTION_MAGNITUDE` uses it:
/// reject any value containing `-` outright (`strtoumax` would accept it), then
/// `strtoumax(value, &end, 0)` — leading whitespace skipped, optional `+`, base
/// detected from a `0x`/`0` prefix — followed by an optional `k`/`m`/`g` factor
/// which must reach the end of the string.
fn parse_magnitude(value: &str) -> Result<u64, MagnitudeError> {
    if value.is_empty() {
        return Err(MagnitudeError::Empty);
    }
    if value.contains('-') {
        return Err(MagnitudeError::Malformed);
    }

    let b = value.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    if i < b.len() && b[i] == b'+' {
        i += 1;
    }

    let radix = if b[i..].starts_with(b"0x") || b[i..].starts_with(b"0X") {
        i += 2;
        16
    } else if b.get(i) == Some(&b'0') {
        8 // `strtoumax` base 0 reads a leading zero as octal; "0" itself is 0.
    } else {
        10
    };

    let start = i;
    let mut number: u64 = 0;
    while let Some(digit) = b.get(i).and_then(|c| (*c as char).to_digit(radix)) {
        number = number
            .checked_mul(u64::from(radix))
            .and_then(|n| n.checked_add(u64::from(digit)))
            .ok_or(MagnitudeError::Range)?;
        i += 1;
    }
    if i == start {
        // `end == value`: no digits consumed at all.
        return Err(MagnitudeError::Malformed);
    }

    // git's `git_parse_unit_factor`: exactly one unit letter, then end of string.
    let factor: u64 = match &b[i..] {
        b"" => 1,
        b"k" | b"K" => 1024,
        b"m" | b"M" => 1024 * 1024,
        b"g" | b"G" => 1024 * 1024 * 1024,
        _ => return Err(MagnitudeError::Malformed),
    };
    number.checked_mul(factor).ok_or(MagnitudeError::Range)
}

/// Whether the repository is a partial clone, i.e. git's `repo_has_promisor_remote`:
/// the `extensions.partialClone` key names a remote, or some remote is marked
/// `remote.<name>.promisor = true`.
fn has_promisor_remote(repo: &gix::Repository) -> bool {
    let config = repo.config_snapshot();
    if config.string("extensions.partialclone").is_some() {
        return true;
    }
    repo.remote_names().iter().any(|name| {
        let key = format!("remote.{}.promisor", name.to_str_lossy());
        config.boolean(&key).unwrap_or(false)
    })
}
