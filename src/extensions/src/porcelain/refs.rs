//! `git refs` — low-level access to refs.
//!
//! Covered, byte-identically with stock git:
//!   * `git refs exists <ref>` — exact ref-store lookup (no rev-parse DWIM), exit
//!     0 when present, 2 when missing, 1 when the lookup itself fails.
//!   * `git refs list ...` — an alias for `git for-each-ref`, dispatched to that
//!     module exactly as `builtin/refs.c` calls `cmd_for_each_ref()`.
//!   * `git refs optimize ...` — an alias for `git pack-refs`, dispatched to that
//!     module as `builtin/refs.c` calls `cmd_pack_refs()`.
//!   * the subcommand dispatch itself: `-h` (usage on stdout, exit 129), a
//!     missing subcommand (`error: need a subcommand` + usage on stderr, 129),
//!     an unknown subcommand, and each subcommand's own `-h` usage block.
//!
//! Not covered, and rejected with an error rather than approximated:
//!   * `git refs migrate` — writing the reftable format. The vendored `gix-ref`
//!     has no reftable backend at all (its `store/` holds only the loose+packed
//!     files backend), so there is nothing to migrate to or from.
//!   * `git refs verify` — ref-database consistency checking. The vendored
//!     `gix-fsck` covers object-graph connectivity only (`Connectivity`,
//!     `check_commit`); no ref-name, symref-target or packed-refs checker exists
//!     in the vendored crates, and a `verify` that silently exits 0 would report
//!     a healthy database it never inspected.
//!
//! Known divergence: usage *errors* raised inside `optimize` are reported by the
//! `pack-refs` module, so their usage block reads `usage: git pack-refs ...`
//! where git would print `usage: git refs optimize ...`. `refs optimize -h` is
//! handled here and does print the `git refs optimize` form.

use anyhow::{bail, Result};
use std::process::ExitCode;

/// git's top-level `git refs` usage block, reproduced byte-for-byte (it is part
/// of the output contract for `-h` on stdout and for dispatch errors on stderr).
const USAGE: &str = "\
usage: git refs migrate --ref-format=<format> [--no-reflog] [--dry-run]\n\
\x20  or: git refs verify [--strict] [--verbose]\n\
\x20  or: git refs list [--count=<count>] [--shell|--perl|--python|--tcl]\n\
\x20                               [(--sort=<key>)...] [--format=<format>]\n\
\x20                               [--include-root-refs] [--points-at=<object>]\n\
\x20                               [--merged[=<object>]] [--no-merged[=<object>]]\n\
\x20                               [--contains[=<object>]] [--no-contains[=<object>]]\n\
\x20                               [(--exclude=<pattern>)...] [--start-after=<marker>]\n\
\x20                               [ --stdin | (<pattern>...)]\n\
\x20  or: git refs exists <ref>\n\
\x20  or: git refs optimize [--all] [--no-prune] [--auto] [--include <pattern>] [--exclude <pattern>]\n\
\n\
";

/// `git refs list -h`, byte-for-byte.
const USAGE_LIST: &str = "\
usage: git refs list [--count=<count>] [--shell|--perl|--python|--tcl]\n\
\x20                               [(--sort=<key>)...] [--format=<format>]\n\
\x20                               [--include-root-refs] [--points-at=<object>]\n\
\x20                               [--merged[=<object>]] [--no-merged[=<object>]]\n\
\x20                               [--contains[=<object>]] [--no-contains[=<object>]]\n\
\x20                               [(--exclude=<pattern>)...] [--start-after=<marker>]\n\
\x20                               [ --stdin | (<pattern>...)]\n\
\n\
\x20   -s, --[no-]shell      quote placeholders suitably for shells\n\
\x20   -p, --[no-]perl       quote placeholders suitably for perl\n\
\x20   --[no-]python         quote placeholders suitably for python\n\
\x20   --[no-]tcl            quote placeholders suitably for Tcl\n\
\x20   --[no-]omit-empty     do not output a newline after empty formatted refs\n\
\n\
\x20   --[no-]count <n>      show only <n> matched refs\n\
\x20   --[no-]format <format>\n\
\x20                         format to use for the output\n\
\x20   --[no-]start-after <marker>\n\
\x20                         start iteration after the provided marker\n\
\x20   --[no-]color[=<when>] respect format colors\n\
\x20   --[no-]exclude <pattern>\n\
\x20                         exclude refs which match pattern\n\
\x20   --[no-]sort <key>     field name to sort on\n\
\x20   --[no-]points-at <object>\n\
\x20                         print only refs which points at the given object\n\
\x20   --merged <commit>     print only refs that are merged\n\
\x20   --no-merged <commit>  print only refs that are not merged\n\
\x20   --contains <commit>   print only refs which contain the commit\n\
\x20   --no-contains <commit>\n\
\x20                         print only refs which don't contain the commit\n\
\x20   --[no-]ignore-case    sorting and filtering are case insensitive\n\
\x20   --[no-]stdin          read reference patterns from stdin\n\
\x20   --[no-]include-root-refs\n\
\x20                         also include HEAD ref and pseudorefs\n\
\n\
";

/// `git refs optimize -h`, byte-for-byte.
const USAGE_OPTIMIZE: &str = "\
usage: git refs optimize [--all] [--no-prune] [--auto] [--include <pattern>] [--exclude <pattern>]\n\
\n\
\x20   --[no-]all            pack everything\n\
\x20   --[no-]prune          prune loose refs (default)\n\
\x20   --[no-]auto           auto-pack refs as needed\n\
\x20   --[no-]include <pattern>\n\
\x20                         references to include\n\
\x20   --[no-]exclude <pattern>\n\
\x20                         references to exclude\n\
\n\
";

/// `git refs exists -h`, byte-for-byte.
const USAGE_EXISTS: &str = "\
usage: git refs exists <ref>\n\
\n\
";

/// Ref-name prefixes whose refs live in the per-worktree `$GIT_DIR` rather than
/// in the shared `$GIT_COMMON_DIR`, per git's `is_per_worktree_ref()`.
const PER_WORKTREE: [&str; 3] = ["refs/worktree/", "refs/bisect/", "refs/rewritten/"];

/// `git refs` — see the module docs for the covered surface.
pub fn refs(args: &[String]) -> Result<ExitCode> {
    // `args[0]` is the subcommand name `refs` itself, as dispatch passes it.
    let Some(sub) = args.get(1) else {
        eprint!("error: need a subcommand\n{USAGE}");
        return Ok(ExitCode::from(129));
    };

    match sub.as_str() {
        "-h" => {
            print!("{USAGE}");
            Ok(ExitCode::from(129))
        }
        "exists" => exists(&args[2..]),
        "list" => list(&args[2..]),
        "optimize" => optimize(&args[1..]),
        "migrate" => bail!(
            "unsupported subcommand \"migrate\": the vendored gix-ref has no reftable backend \
             (only the loose+packed files store), so there is no format to migrate to"
        ),
        "verify" => bail!(
            "unsupported subcommand \"verify\": the vendored crates have no ref-database \
             consistency checker (gix-fsck covers object connectivity only)"
        ),
        // git's option parser reports an unknown leading dashed argument before
        // it ever looks for a subcommand.
        s if s.starts_with("--") => {
            eprintln!("error: unknown option `{}'", &s[2..]);
            eprint!("{USAGE}");
            Ok(ExitCode::from(129))
        }
        s if s.starts_with('-') && s.len() > 1 => {
            eprintln!("error: unknown switch `{}'", &s[1..2]);
            eprint!("{USAGE}");
            Ok(ExitCode::from(129))
        }
        s => {
            eprintln!("error: unknown subcommand: `{s}'");
            eprint!("{USAGE}");
            Ok(ExitCode::from(129))
        }
    }
}

/// `git refs exists <ref>` — is `<ref>` present in the ref database?
///
/// Exit 0 when it is, 2 when it is not, 1 when the lookup failed for a reason
/// other than the ref being absent. Whether the ref resolves to a real object is
/// deliberately not checked, matching git: a symref pointing at a missing branch
/// and a ref holding an unknown object id both exist.
fn exists(args: &[String]) -> Result<ExitCode> {
    let mut name: Option<&str> = None;
    let mut positionals = 0usize;
    let mut end_of_opts = false;

    for a in args {
        if !end_of_opts && a == "--" {
            end_of_opts = true;
            continue;
        }
        if !end_of_opts && a.len() > 1 && a.starts_with('-') {
            if a == "-h" {
                print!("{USAGE_EXISTS}");
                return Ok(ExitCode::from(129));
            }
            if let Some(long) = a.strip_prefix("--") {
                eprintln!("error: unknown option `{long}'");
            } else {
                eprintln!("error: unknown switch `{}'", &a[1..2]);
            }
            eprint!("{USAGE_EXISTS}");
            return Ok(ExitCode::from(129));
        }
        positionals += 1;
        name = Some(a.as_str());
    }

    // git demands exactly one reference; zero or two or more is a usage fatal.
    let (Some(name), 1) = (name, positionals) else {
        eprintln!("fatal: 'git refs exists' requires a reference");
        return Ok(ExitCode::from(128));
    };

    let repo = gix::discover(".")?;
    match lookup(&repo, name) {
        Ok(true) => Ok(ExitCode::SUCCESS),
        Ok(false) => {
            eprintln!("error: reference does not exist");
            Ok(ExitCode::from(2))
        }
        // Documented as distinct from "missing": a failed lookup exits 1.
        Err(err) => {
            eprintln!("error: {err}");
            Ok(ExitCode::from(1))
        }
    }
}

/// Whether `name` names an existing ref, looked up *verbatim*.
///
/// git's `refs_ref_exists()` performs no rev-parse DWIM, so `master` does not
/// find `refs/heads/master` — hence this bypasses `gix`'s partial-name search
/// (which walks `refs/`, `refs/tags/`, `refs/heads/`, `refs/remotes/`) and reads
/// the one loose path the name maps to, falling back to `packed-refs`.
///
/// A name that is not a valid full ref name is reported as missing, not as an
/// error, which is what git does for e.g. `refs/heads/../x`; it also keeps the
/// name from being joined onto a path it could escape.
fn lookup(repo: &gix::Repository, name: &str) -> Result<bool> {
    if gix::refs::FullName::try_from(name).is_err() {
        return Ok(false);
    }

    let store = &repo.refs;
    // Pseudorefs (single-component names such as `HEAD`, `ORIG_HEAD`) and the
    // per-worktree prefixes live in `$GIT_DIR`; everything else is shared.
    let per_worktree =
        !name.contains('/') || PER_WORKTREE.iter().any(|prefix| name.starts_with(prefix));
    let base = if per_worktree {
        store.git_dir()
    } else {
        store.common_dir_resolved()
    };
    if base.join(name).is_file() {
        return Ok(true);
    }

    // Only `refs/`-rooted names can ever appear in `packed-refs`.
    if !name.starts_with("refs/") {
        return Ok(false);
    }
    let Some(packed) = store.open_packed_buffer()? else {
        return Ok(false);
    };
    Ok(packed.try_find(name)?.is_some())
}

/// `git refs list` — the documented alias for `git for-each-ref`.
///
/// Handled here: the `-h` usage block, and the `-s` short form of `--shell`,
/// which `for-each-ref` itself does not accept. Everything else is passed
/// through unchanged, so the covered flag set is exactly that module's.
fn list(args: &[String]) -> Result<ExitCode> {
    // The subcommand name lets `for_each_ref` strip index 0 unconditionally, so
    // a pattern that happens to read `for-each-ref` is never mistaken for it.
    let mut forwarded: Vec<String> = vec!["for-each-ref".to_string()];
    let mut end_of_opts = false;

    for a in args {
        if !end_of_opts {
            if a == "--" {
                end_of_opts = true;
            } else if a == "-h" {
                print!("{USAGE_LIST}");
                return Ok(ExitCode::from(129));
            } else if a == "-s" {
                forwarded.push("--shell".to_string());
                continue;
            }
        }
        forwarded.push(a.clone());
    }

    super::for_each_ref::for_each_ref(&forwarded)
}

/// `git refs optimize` — the documented alias for `git pack-refs`.
///
/// `args[0]` is the literal `optimize`, which the `pack-refs` module skips just
/// as it skips its own subcommand name. Only `-h` is intercepted, so that the
/// usage block names `git refs optimize` rather than `git pack-refs`.
fn optimize(args: &[String]) -> Result<ExitCode> {
    if args[1..].iter().any(|a| a == "-h") {
        print!("{USAGE_OPTIMIZE}");
        return Ok(ExitCode::from(129));
    }
    super::pack_refs::pack_refs(args)
}
