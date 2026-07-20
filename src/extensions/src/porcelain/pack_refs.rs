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
//! Not covered, and rejected with an error rather than approximated: `--auto`.
//! Its threshold heuristic lives in git's ref backends, is explicitly documented
//! as subject to change, and has no counterpart in the vendored `gix-ref`;
//! guessing it would silently pack (or not pack) at the wrong times.
//!
//! Two behaviours git has that `gix-ref`'s packed transaction does not are
//! reproduced here explicitly: git removes the now-empty parent directories left
//! behind by pruning (but never `refs/<top>` itself), and git always leaves a
//! `packed-refs` file behind — header-only when nothing was packed.

use anyhow::{bail, Result};
use std::path::Path;
use std::process::ExitCode;

use gix::bstr::{BStr, ByteSlice};
use gix::glob::wildmatch;
use gix::glob::wildmatch::Mode;
use gix::hash::ObjectId;
use gix::refs::file::transaction::PackedRefs;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// The first line of a `packed-refs` file, byte-identical to what git writes.
const HEADER_LINE: &[u8] = b"# pack-refs with: peeled fully-peeled sorted \n";

/// Ref name prefixes that are per-worktree and therefore never packed.
const PER_WORKTREE: [&str; 3] = ["refs/bisect/", "refs/worktree/", "refs/rewritten/"];

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
    all: bool,               // --all: add `*` to the include set
    prune: bool,             // --prune (default): delete the loose ref once packed
    includes: Vec<String>,   // --include: accumulated inclusion patterns
    excludes: Vec<String>,   // --exclude: accumulated exclusion patterns
}

/// `git pack-refs` — see the module docs for the covered surface.
pub fn pack_refs(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts {
        all: false,
        prune: true,
        includes: Vec::new(),
        excludes: Vec::new(),
    };

    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--all" => opts.all = true,
            "--no-all" => opts.all = false,
            "--prune" => opts.prune = true,
            "--no-prune" => opts.prune = false,
            "--no-auto" => {} // the default; nothing to do
            "--auto" => bail!("unsupported flag \"--auto\" (ported: --all, --no-all, --prune, --no-prune, --include, --no-include, --exclude, --no-exclude)"),
            "--" => {} // end of options; `pack-refs` takes no positionals anyway
            "--no-include" => opts.includes.clear(),
            "--no-exclude" => opts.excludes.clear(),
            "--include" | "--exclude" => {
                let name = &a[2..];
                let Some(value) = args.get(i + 1) else {
                    eprintln!("error: option `{name}' requires a value");
                    eprint!("{USAGE}");
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

    let repo = gix::discover(".")?;
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
        if !selected(reference.name.as_bstr(), &includes, &opts.excludes) {
            continue;
        }
        // A ref pointing at a missing object is broken and is left alone.
        if !repo.has_object(oid) {
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

    if !edits.is_empty() {
        let objects: Box<dyn gix::objs::Find + '_> = Box::new(&repo.objects);
        let mode = if opts.prune {
            PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(objects)
        } else {
            PackedRefs::DeletionsAndNonSymbolicUpdates(objects)
        };
        store
            .transaction()
            .packed_refs(mode)
            .prepare(
                edits,
                gix::lock::acquire::Fail::Immediately,
                gix::lock::acquire::Fail::Immediately,
            )?
            .commit(None::<gix::actor::SignatureRef<'_>>)?;
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
