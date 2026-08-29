//! `git pack-refs` — move loose refs into `$GIT_DIR/packed-refs`.
//!
//! Covered: `--all`/`--no-all`, `--prune`/`--no-prune` (prune is the default),
//! `--include <pattern>`/`--include=<pattern>`/`--no-include`,
//! `--exclude <pattern>`/`--exclude=<pattern>`/`--no-exclude`, and `-h`.
//! Stock git prints nothing on success and exits 0; so does this. Usage errors
//! print git's own usage block and exit 129.
//!
//! The selection rules follow `should_pack_ref()` in git's files backend: the
//! default include set is `refs/tags/*`, `--all` adds `*`, `--exclude` wins over
//! `--include`, per-worktree refs (`refs/bisect/`, `refs/worktree/`,
//! `refs/rewritten/`), symbolic refs and broken refs are never packed. Patterns
//! are matched with `wildmatch` in git's mode 0, so `*` crosses `/` — which is
//! why `refs/tags/*` packs `refs/tags/a/b`.
//!
//! `--auto` is implemented as a direct port of `should_pack_refs()` in git's
//! files backend: the run is skipped entirely unless the number of packable
//! loose refs reaches `max(16, log2(packed_refs_size / 100) * 5)`. When the
//! threshold is not met git returns before opening the packed transaction, so
//! no `packed-refs` file is created either.
//!
//! Two behaviours git has that `gix-ref`'s packed transaction does not are
//! reproduced here explicitly: git removes the now-empty parent directories left
//! behind by pruning (but never `refs/<top>` itself), and git always leaves a
//! `packed-refs` file behind — header-only when nothing was packed.

use anyhow::Result;
use std::path::Path;
use std::process::ExitCode;

use gix::bstr::{BStr, ByteSlice};
use gix::glob::wildmatch;
use gix::glob::wildmatch::Mode;
use gix::hash::ObjectId;
use gix::refs::file::transaction::PackedRefs;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

use super::{Arg, LongOpt};

/// The first line of a `packed-refs` file, byte-identical to what git writes.
const HEADER_LINE: &[u8] = b"# pack-refs with: peeled fully-peeled sorted \n";

/// Ref name prefixes that are per-worktree and therefore never packed.
const PER_WORKTREE: [&str; 3] = ["refs/bisect/", "refs/worktree/", "refs/rewritten/"];

/// `cmd_pack_refs()`'s `struct option opts[]` (builtin/pack-refs.c), in table
/// order, as [`super::resolve_long`] reads it. No entry carries
/// `PARSE_OPT_NONEG`.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "all", neg: true, arg: Arg::None },
    LongOpt { name: "prune", neg: true, arg: Arg::None },
    LongOpt { name: "auto", neg: true, arg: Arg::None },
    LongOpt { name: "include", neg: true, arg: Arg::Required },
    LongOpt { name: "exclude", neg: true, arg: Arg::Required },
];

/// git's own usage block, reproduced byte-for-byte (it is part of the output
/// contract for `-h` on stdout and for usage errors on stderr).
const USAGE: &str = "usage: git pack-refs [--all] [--no-prune] [--auto] [--include <pattern>] [--exclude <pattern>]

    --[no-]all            pack everything
    --[no-]prune          prune loose refs (default)
    --[no-]auto           auto-pack refs as needed
    --[no-]include <pattern>
                          references to include
    --[no-]exclude <pattern>
                          references to exclude

";

/// Parsed command-line options for a single `pack-refs` invocation.
struct Opts {
    all: bool,             // --all: add `*` to the include set
    prune: bool,           // --prune (default): delete the loose ref once packed
    auto: bool,            // --auto: only pack once the loose-ref threshold is reached
    includes: Vec<String>, // --include: accumulated inclusion patterns
    excludes: Vec<String>, // --exclude: accumulated exclusion patterns
}

/// `git pack-refs` — see the module docs for the covered surface.
pub fn pack_refs(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts {
        all: false,
        prune: true,
        auto: false,
        includes: Vec::new(),
        excludes: Vec::new(),
    };

    // `dispatch::run` splits the subcommand off and hands us only the arguments,
    // so parsing starts at index 0. The two in-tree callers — `gc` and
    // `refs optimize` — instead pass their own verb as `args[0]`, so that one
    // token is skipped when it is literally `pack-refs` or `optimize`. Neither
    // word is a valid `pack-refs` argument (stock git answers both with the
    // usage block), so no argument git would accept is swallowed by this.
    let mut i = usize::from(matches!(
        args.first().map(String::as_str),
        Some("pack-refs" | "optimize")
    ));
    // Everything after a literal `--` is a positional, and `pack-refs` accepts
    // none, so it is a usage error rather than a flag.
    let mut positional_only = false;
    while i < args.len() {
        let a = args[i].as_str();
        if positional_only {
            eprint!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`, so it is matched before the abbreviation resolution
        // below rather than added to `LONG_OPTS`. This table has no
        // `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` renders the same block `-h`
        // prints.
        if a == "--help-all" {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        let resolved = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => name,
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };
        let a = resolved.as_ref();
        match a {
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--all" => opts.all = true,
            "--no-all" => opts.all = false,
            "--prune" => opts.prune = true,
            "--no-prune" => opts.prune = false,
            "--auto" => opts.auto = true,
            "--no-auto" => opts.auto = false,
            "--" => positional_only = true,
            "--no-include" => opts.includes.clear(),
            "--no-exclude" => opts.excludes.clear(),
            "--include" | "--exclude" => {
                let name = &a[2..];
                let Some(value) = args.get(i + 1) else {
                    // `case PARSE_OPT_ERROR: exit(129);` in `parse_options()` — the usage
                    // block belongs to `PARSE_OPT_HELP`, not to a missing value.
                    eprintln!("error: option `{name}' requires a value");
                    return Ok(ExitCode::from(129));
                };
                i += 1;
                if name == "include" {
                    opts.includes.push(value.clone());
                } else {
                    opts.excludes.push(value.clone());
                }
            }
            _ if a.starts_with("--include=") => opts.includes.push(a["--include=".len()..].to_string()),
            _ if a.starts_with("--exclude=") => opts.excludes.push(a["--exclude=".len()..].to_string()),
            _ if a.starts_with("--") => return usage_error(&format!("unknown option `{}'", &a[2..])),
            _ if a.starts_with('-') && a.len() > 1 => {
                return usage_error(&format!("unknown switch `{}'", &a[1..2]))
            }
            // `pack-refs` takes no positional arguments; git prints usage and exits 129.
            _ => {
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        }
        i += 1;
    }

    // git appends `*` for --all, then falls back to `refs/tags/*` when the
    // include set is still empty. Hence `--all --include <p>` is a no-op for <p>.
    let mut includes = opts.includes;
    if opts.all {
        includes.push("*".to_string());
    }
    if includes.is_empty() {
        includes.push("refs/tags/*".to_string());
    }

    let repo = crate::setup::discover()?;
    let store = &repo.refs;

    // Only loose refs need an edit: refs that already live in `packed-refs` and
    // are not shadowed by a loose file are carried over verbatim by the packed
    // transaction, which merges its edits into the existing sorted entries.
    let mut edits: Vec<RefEdit> = Vec::new();
    let mut packed_names: Vec<FullName> = Vec::new();
    for reference in store.loose_iter()? {
        // A ref that fails to parse is a broken ref; git skips those silently.
        let Ok(reference) = reference else { continue };
        // Symbolic refs cannot be represented in `packed-refs`.
        let Some(oid) = reference.target.try_id().map(ObjectId::from) else {
            continue;
        };
        // ```c
        // /* Do not pack broken refs: */
        // if (!ref_resolves_to_object(refname, refs->base.repo, oid, ref_flags))
        //         return 0;
        //
        // if (ref_excluded(opts->exclusions, refname))
        //         return 0;
        //
        // for_each_string_list_item(item, opts->includes)
        //         if (!wildmatch(item->string, refname, 0))
        //                 return 1;
        // ```
        //
        // (`should_pack_ref()`, refs/files-backend.c.) The object check comes *before* the
        // include/exclude filter, so a dangling ref is reported —
        // `ref_resolves_to_object()`'s `error(_("%s does not point to a valid object!"))`
        // — whether or not the patterns would have selected it.
        if !repo.has_object(oid) {
            eprintln!("error: {} does not point to a valid object!", reference.name.as_bstr());
            continue;
        }
        if !selected(reference.name.as_bstr(), &includes, &opts.excludes) {
            continue;
        }
        edits.push(RefEdit {
            change: Change::Update {
                // `MustExistAndMatch` with the value we are about to write means
                // the reflog append is suppressed — packing must not add log
                // entries, and git does not add any either.
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: Default::default(),
                },
                expected: PreviousValue::MustExistAndMatch(Target::Object(oid)),
                new: Target::Object(oid),
            },
            name: reference.name.clone(),
            deref: false,
        });
        packed_names.push(reference.name);
    }

    // `should_pack_refs()`: under `--auto` a run below the threshold returns
    // before the packed transaction is opened, so nothing is written or pruned
    // and no `packed-refs` file is created.
    if opts.auto {
        let packed_refs = store.packed_refs_path();
        if edits.len() < auto_limit(&packed_refs) {
            return Ok(ExitCode::SUCCESS);
        }
    }

    // `files_pack_refs()` takes `packed-refs.lock` the moment `should_pack_refs()` clears the
    // run, before it has looked at a single loose ref (refs/files-backend.c:1470-1478):
    //
    //     if (!should_pack_refs(refs, opts))
    //             return 0;
    //     …
    //     packed_refs_lock(refs->packed_ref_store, LOCK_DIE_ON_ERROR, &err);
    //
    // and `should_pack_refs()` returns 1 outright for anything but `--auto`
    // (`:1405-1406`), so the lock is unconditional: a run with nothing left to pack dies
    // exactly like one that had work to do. `LOCK_DIE_ON_ERROR` routes the failure through
    // `die()`, which is why the exit code is 128 rather than 1.
    //
    // A run that has edits takes this same lock inside the packed transaction below, so it is
    // only acquired here for the empty run — and released again right away, since nothing is
    // written through it.
    let packed_refs = store.packed_refs_path();
    if edits.is_empty() {
        if let Err(e) = gix::lock::File::acquire_to_update_resource(
            &packed_refs,
            gix::lock::acquire::Fail::Immediately,
            None,
        ) {
            return Ok(packed_refs_lock_fatal(&packed_refs, &e));
        }
    } else {
        let objects: Box<dyn gix::objs::Find + '_> = Box::new(&repo.objects);
        let mode = if opts.prune {
            PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(objects)
        } else {
            PackedRefs::DeletionsAndNonSymbolicUpdates(objects)
        };
        let prepared = store.transaction().packed_refs(mode).prepare(
            edits,
            gix::lock::acquire::Fail::Immediately,
            gix::lock::acquire::Fail::Immediately,
        );
        match prepared {
            Ok(t) => {
                t.commit(None::<gix::actor::SignatureRef<'_>>)?;
            }
            // The same `packed_refs_lock()` failure, just reached one layer down.
            Err(gix::refs::file::transaction::prepare::Error::PackedTransactionAcquire(e)) => {
                return Ok(packed_refs_lock_fatal(&packed_refs, &e));
            }
            Err(e) => return Err(e.into()),
        }
    }

    if opts.prune {
        let base = store.common_dir_resolved().to_owned();
        for name in &packed_names {
            if let Ok(name) = name.as_bstr().to_str() {
                remove_empty_parents(&base, name);
            }
        }
    }

    // git rewrites `packed-refs` unconditionally, so even a run that packs
    // nothing leaves a header-only file behind. `gix-ref` skips the write (and
    // deletes the file when it would be empty), so restore that state here.
    let path = store.packed_refs_path();
    if !path.exists() {
        std::fs::write(&path, HEADER_LINE)?;
    }

    Ok(ExitCode::SUCCESS)
}

/// Report a `packed-refs` lock failure the way `LOCK_DIE_ON_ERROR` does, and exit 128.
///
/// `packed_refs_lock()` hands `LOCK_DIE_ON_ERROR` to `hold_lock_file_for_update_timeout()`
/// (refs/packed-backend.c:1235-1241), so the failure never returns to the caller: it goes
/// through `unable_to_lock_die()`, which prints `unable_to_lock_message()` via `die()`.
/// That is the `fatal: ` prefix and the 128.
///
/// [`gix::lock::acquire::Error::PermanentlyLocked`] is raised for the `EEXIST` branch and
/// nothing else — every other `errno` becomes `Error::Io` (gix-lock/src/acquire.rs:257-262) —
/// so the `errno` `unable_to_lock_message()` wants is reconstructed here rather than carried
/// through the error type. `EEXIST` is also the only branch that gets git's two-paragraph form
/// with the holder diagnostic.
pub(super) fn packed_refs_lock_fatal(packed_refs: &Path, err: &gix::lock::acquire::Error) -> ExitCode {
    let message = match err {
        gix::lock::acquire::Error::Io(err) => gix::lock::pid::unable_to_lock_message(packed_refs, err),
        gix::lock::acquire::Error::PermanentlyLocked { .. } => {
            gix::lock::pid::unable_to_lock_message(packed_refs, &eexist())
        }
    };
    eprintln!("fatal: {message}");
    ExitCode::from(128)
}

/// The `EEXIST` that `gix_lock::acquire::Error::PermanentlyLocked` stands for, as an
/// [`std::io::Error`] carrying the raw `errno`.
///
/// It has to be the raw code rather than [`std::io::ErrorKind::AlreadyExists`]: git renders it
/// with `strerror(errno)`, and `unable_to_lock_message()`'s `strerror` helper
/// (gix-lock/src/pid.rs:109-118) only reaches the platform string through
/// `raw_os_error()`. A kind-only error renders as Rust's own "entity already exists" instead of
/// git's "File exists".
pub(super) fn eexist() -> std::io::Error {
    std::io::Error::from_raw_os_error(libc::EEXIST)
}

/// The number of packable loose refs `--auto` requires before it packs at all,
/// ported from `should_pack_refs()` in git's files backend.
///
/// git weighs the cost of rewriting `packed-refs` against how much churn a
/// repository sees, estimating the packed ref count as `size / 100` and allowing
/// `log2(count) * 5` loose refs on top of it — roughly 16 more per factor of ten
/// — with a floor of 16. A missing `packed-refs` file counts as size 0.
fn auto_limit(packed_refs: &Path) -> usize {
    let size = std::fs::metadata(packed_refs).map_or(0, |m| m.len() as usize);
    (log2u(size / 100) * 5).max(16)
}

/// git's `log2u()`: the floor of the base-2 logarithm, with `log2u(0) == 0`.
fn log2u(n: usize) -> usize {
    if n == 0 {
        0
    } else {
        usize::BITS as usize - 1 - n.leading_zeros() as usize
    }
}

/// Report a usage error the way git's option parser does, then exit 129.
fn usage_error(message: &str) -> Result<ExitCode> {
    eprintln!("error: {message}");
    eprint!("{USAGE}");
    Ok(ExitCode::from(129))
}

/// Whether `name` is packed, per git's `should_pack_ref()`.
///
/// Exclusions win over inclusions, and per-worktree refs are never candidates.
/// Patterns use `wildmatch` with no flags, so `*` spans `/` just as it does in
/// git — `refs/tags/*` therefore selects `refs/tags/a/b`.
fn selected(name: &BStr, includes: &[String], excludes: &[String]) -> bool {
    if PER_WORKTREE
        .iter()
        .any(|prefix| name.starts_with(prefix.as_bytes()))
    {
        return false;
    }
    let matches = |pattern: &String| wildmatch(pattern.as_bytes().as_bstr(), name, Mode::empty());
    !excludes.iter().any(matches) && includes.iter().any(matches)
}

/// Delete the directories a pruned loose ref left empty, mirroring git's
/// `try_remove_empty_parents()`.
///
/// git skips the first two components of the ref name, so `refs/heads` and
/// `refs/tags` always survive while `refs/remotes/origin` or
/// `refs/heads/deep/nested` are removed. Removal stops at the first directory
/// that is not empty.
fn remove_empty_parents(base: &Path, name: &str) {
    let parts: Vec<&str> = name.split('/').collect();
    for i in (3..parts.len()).rev() {
        if std::fs::remove_dir(base.join(parts[..i].join("/"))).is_err() {
            break;
        }
    }
}
