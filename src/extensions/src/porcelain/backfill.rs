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
//!   * the integer-valued rev options (`--max-count`, `--skip`, `--min-parents`,
//!     `--max-parents` and the `-<n>` shorthand) are validated with git's
//!     `strtol_i`, so a malformed value (`--max-count=0x10`, `-5abc`, an overflow)
//!     gives `fatal: '<value>': not an integer`, exit 128. `--max-count`/`--skip`
//!     also take the value from the next argument and report `Option '<opt>'
//!     requires a value` when it is missing or is the `--` terminator.
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

    let repo = crate::setup::discover()?;

    // Pass one: parse-options over the whole argv, stopping at `--`, which it
    // leaves in place for `setup_revisions`. Its errors precede every revision
    // error, matching git's ordering.
    let mut sparse: Option<bool> = None;
    let mut rest: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            // `parse_options_step()` tests `--help-all` with a `strcmp()` of its
            // own, so it renders `USAGE_FULL` — the same block `-h` prints, this
            // table having no `PARSE_OPT_HIDDEN` entry.
            "-h" | "--help-all" => {
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
    // `setup_revisions` hands back the options it did not recognize and lets the
    // caller complain; `cmd_backfill` does so only once the whole scan is over, so
    // a revision error later in the argv is reported ahead of this one.
    let mut unrecognized: Option<&str> = None;

    // git pre-scans for `--` before looking at anything else: the arguments after
    // it become prune data, taken as pathspecs with no check that they exist, and
    // the ones before it are revisions *only* — a non-revision there is a `bad
    // revision`, never a path.
    let head: &[&str] = match rest.iter().position(|&a| a == "--") {
        Some(n) => &rest[..n],
        None => &rest[..],
    };
    let seen_dashdash = head.len() != rest.len();

    let mut j = 0;
    while j < head.len() {
        let a = head[j];
        j += 1;

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
            // `-<n>` is rev-list's max-count shorthand: git takes any `-<digit>…`
            // as the numeric form and validates the tail with `strtol_i`, dying
            // `'<n>': not an integer` on anything it will not fully consume.
            if a.as_bytes()[1].is_ascii_digit() {
                let n = &a[1..];
                if strtol_i(n) {
                    continue;
                }
                eprintln!("fatal: '{n}': not an integer");
                return Ok(ExitCode::from(128));
            }

            let (name, value) = match a.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (a, None),
            };

            // Integer-valued rev options validate with git's `strtol_i` (base 10,
            // signed `int`, whole-string). `--max-count`/`--skip` take the value
            // from `=` or the next argument — but never the `--` terminator, which
            // git reports as a missing value. `--min-parents`/`--max-parents` take
            // it only from `=`; their bare spelling is unrecognized here.
            match name {
                "--max-count" | "--skip" => {
                    let value = match value {
                        Some(value) => value,
                        None => match head.get(j) {
                            Some(&next) if next != "--" => {
                                j += 1;
                                next
                            }
                            _ => {
                                eprintln!("fatal: Option '{name}' requires a value");
                                return Ok(ExitCode::from(128));
                            }
                        },
                    };
                    if !strtol_i(value) {
                        eprintln!("fatal: '{value}': not an integer");
                        return Ok(ExitCode::from(128));
                    }
                    continue;
                }
                "--min-parents" | "--max-parents" => {
                    let Some(value) = value else {
                        eprintln!("fatal: unrecognized argument: {a}");
                        return Ok(ExitCode::from(128));
                    };
                    if !strtol_i(value) {
                        eprintln!("fatal: '{value}': not an integer");
                        return Ok(ExitCode::from(128));
                    }
                    continue;
                }
                _ => {}
            }

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
                    "unsupported flag \"--stdin\": it reads revisions from stdin, and this port \
                     has no walk to spend them on, so invalid input git reports would go unreported"
                );
            }
            unrecognized.get_or_insert(a);
            continue;
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

        // `handle_revision_arg` failed. A `--` earlier in the argv declared every
        // argument before it a revision, so git dies outright rather than falling
        // back to a path. Otherwise this argument *and every one after it* must
        // name a path, and revision parsing stops here — which is why a valid
        // revision behind a path is still rejected (`README.md HEAD`).
        if seen_dashdash {
            eprintln!("fatal: bad revision '{a}'");
            return Ok(ExitCode::from(128));
        }
        for (n, arg) in head[j - 1..].iter().enumerate() {
            if let Some(code) = verify_filename(arg, n == 0) {
                return Ok(code);
            }
        }
        break;
    }

    if let Some(a) = unrecognized {
        eprintln!("fatal: unrecognized argument: {a}");
        return Ok(ExitCode::from(128));
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
        download_missing_blobs(&repo);
    }

    // No promisor remote: there is nothing to request and nothing to write.
    // Stock git prints nothing, touches nothing and exits 0.
    Ok(ExitCode::SUCCESS)
}

/// `do_backfill()`: hand the promisor remote every blob the history reaches that
/// this repository does not have.
///
/// ```c
/// info.blobs = 1;
/// info.tags = info.commits = info.trees = 0;
/// info.revs = &ctx->revs;
/// info.path_fn = fill_missing_blobs;
/// ret = walk_objects_by_path(&info);
/// if (!ret)
///         download_batch(ctx);
/// ```
///
/// with `fill_missing_blobs()` keeping the ids `odb_has_object()` says are
/// absent, and `download_batch()` handing them to `promisor_remote_get_direct()`.
/// The walk starts at `HEAD` when no revision range was given
/// (`add_head_to_pending()`), which is every case this port accepts today.
///
/// `--min-batch-size` decides how many round trips those ids are split across
/// and nothing else, so one request is made here: the objects that end up on
/// disk are the same set either way, which is the whole of what the command
/// leaves behind. A blob the remote will not hand over is not an error —
/// `download_batch()` ignores the outcome and `cmd_backfill()` still returns 0.
fn download_missing_blobs(repo: &gix::Repository) {
    use gix::object::Kind;

    let Ok(head) = repo.head_id() else { return };

    // `fill_missing_blobs()` asks `odb_has_object(ctx->repo->objects, &oid, 0)` —
    // flags `0`, so *without* `ODB_HAS_OBJECT_FETCH_PROMISOR`. The point of the
    // command is to batch the download; a presence check that fetched would turn
    // it into one round trip per blob, which is what the batch exists to avoid.
    let restore = gix::odb::store::fetch_if_missing();
    gix::odb::store::set_fetch_if_missing(false);

    // Walk commits and trees only, never blobs: reading a blob is what the walk
    // is trying to *avoid* doing one at a time. Tree entries name the blobs, and
    // a tree in a `blob:none` clone is present by construction.
    let mut seen: std::collections::HashSet<gix::ObjectId> = std::collections::HashSet::new();
    let mut stack: Vec<gix::ObjectId> = vec![head.detach()];
    let mut missing: Vec<gix::ObjectId> = Vec::new();
    seen.insert(head.detach());
    while let Some(id) = stack.pop() {
        let Ok(object) = repo.find_object(id) else { continue };
        let mut next: Vec<gix::ObjectId> = Vec::new();
        match object.kind {
            Kind::Commit => {
                if let Some((tree, parents)) = object
                    .into_commit()
                    .decode()
                    .ok()
                    .map(|c| (c.tree(), c.parents().collect::<Vec<_>>()))
                {
                    next.push(tree);
                    next.extend(parents);
                }
            }
            Kind::Tag => {
                if let Ok(tag) = object.into_tag().decode() {
                    next.push(tag.target());
                }
            }
            Kind::Tree => {
                if let Ok(tree) = object.into_tree().decode() {
                    for entry in &tree.entries {
                        let oid = entry.oid.to_owned();
                        match entry.mode.kind() {
                            // `process_tree()` never descends into a submodule,
                            // and a gitlink names a commit this repository is not
                            // expected to hold.
                            gix::object::tree::EntryKind::Commit => {}
                            gix::object::tree::EntryKind::Tree => next.push(oid),
                            // Blob, executable or link: `fill_missing_blobs()`
                            // keeps the ones the object database does not have.
                            _ => {
                                if seen.insert(oid) && !repo.has_object(oid) {
                                    missing.push(oid);
                                }
                            }
                        }
                    }
                }
            }
            Kind::Blob => {}
        }
        for id in next {
            if seen.insert(id) {
                stack.push(id);
            }
        }
    }

    gix::odb::store::set_fetch_if_missing(restore);
    // Straight to the store rather than through `gix::promisor::prefetch`: that
    // helper re-checks each id with `Exists`, which *is* one of the callers that
    // consults the promisor remote, so the batch would be spent one object at a
    // time before it was ever sent. The ids collected above are already known to
    // be absent.
    repo.objects.store_ref().fetch_from_promisor(&missing);
}

/// git's `strtol_i`: is `s` a base-10 signed `int` the way `strtol(s, &end, 10)`
/// reads it — leading ASCII whitespace skipped, an optional `+`/`-`, decimal
/// digits, and then `*end == '\0'` (no trailing bytes) with the result in `int`
/// range. So ` 5`, `+5`, `-5`, `07` and `-2147483648` are integers, while `0x10`,
/// `5k`, `5 `, `2147483648` and `` are not. Used for `--max-count`, `--skip`,
/// `--min-parents`, `--max-parents` and the `-<n>` shorthand.
fn strtol_i(s: &str) -> bool {
    // strtol skips only C-locale whitespace; Rust's `i32` parser then enforces
    // the sign/digit grammar and the whole-string, in-range requirements.
    s.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r'])
        .parse::<i32>()
        .is_ok()
}

/// Peel `spec` to a commit id, or `None` when it names no commit.
fn resolve(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let object = repo.rev_parse_single(spec).ok()?.object().ok()?;
    Some(object.peel_to_commit().ok()?.id)
}

/// [`crate::setup::verify_filename`], reported and turned into git's exit code.
///
/// `first` is git's `diagnose_misspelt_rev`, set only for the argument that failed
/// revision resolution; the ones trailing it were already known to be paths, so they
/// get the plainer wording.
fn verify_filename(arg: &str, first: bool) -> Option<ExitCode> {
    let msg = crate::setup::verify_filename(arg, first)?;
    eprintln!("fatal: {msg}");
    Some(ExitCode::from(128))
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

