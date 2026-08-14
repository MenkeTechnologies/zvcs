//! `git refs` — low-level access to refs.
//!
//! Covered, byte-identically with stock git:
//!   * `git refs exists <ref>` — exact ref-store lookup (no rev-parse DWIM), exit
//!     0 when present, 2 when missing, 1 when the lookup itself fails.
//!   * `git refs list ...` — an alias for `git for-each-ref`, dispatched to that
//!     module exactly as `builtin/refs.c` calls `cmd_for_each_ref()`.
//!   * `git refs optimize ...` — an alias for `git pack-refs`, dispatched to that
//!     module as `builtin/refs.c` calls `cmd_pack_refs()`.
//!   * `git refs verify [--strict] [--verbose]` — ref-database consistency
//!     checking, ported in [`super::fsck::fsck_refs`]: the loose-ref walk, the
//!     root refs, and the `packed-refs` parse, each reporting through the
//!     `fsck.<msg-id>` severities. `git fsck --references` runs this very
//!     command in git, and reaches the same code here.
//!   * the subcommand dispatch itself: `-h` (usage on stdout, exit 129), a
//!     missing subcommand (`error: need a subcommand` + usage on stderr, 129),
//!     an unknown subcommand, and each subcommand's own `-h` usage block.
//!
//!   * `git refs migrate --ref-format=<format>` up to the point where bytes would
//!     move: the option scan, `usage: too many arguments`, `usage: missing
//!     --ref-format=<format>`, `error: unknown ref storage format '<x>'`, and
//!     `error: repository already uses '<x>' format`. `cmd_refs_migrate()` reaches
//!     `repo_migrate_ref_storage_format()` only after all four, and every one of
//!     them is a decision about names and the repository's current format.
//!
//! Not covered, and rejected with an error rather than approximated:
//!   * the migration itself — `git refs migrate --ref-format=reftable` on a repo in
//!     `files` format. The vendored `gix-ref` has no reftable backend at all (its
//!     `store/` holds only the loose+packed files backend), so there is nothing to
//!     migrate to. That same gap is why `verify` never reports
//!     `badReftableTableName`.
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

/// `git refs migrate -h`, byte-for-byte.
const USAGE_MIGRATE: &str = "\
usage: git refs migrate --ref-format=<format> [--no-reflog] [--dry-run]\n\
\n\
\x20   --ref-format <format> specify the reference format to convert to\n\
\x20   --[no-]dry-run        perform a non-destructive dry-run\n\
\x20   --no-reflog           drop reflogs entirely during the migration\n\
\x20   --reflog              opposite of --no-reflog\n\
\n\
";

/// The reference storage backends `ref_storage_format_by_name()` knows, in the order
/// `ref_storage_format_to_name()` reports them back. Matched case-sensitively, as git
/// does — `--ref-format=FILES` is an unknown format, not `files`.
const REF_FORMATS: [&str; 2] = ["files", "reftable"];

/// `git refs verify -h`, byte-for-byte.
const USAGE_VERIFY: &str = "\
usage: git refs verify [--strict] [--verbose]\n\
\n\
\x20   --[no-]verbose        be verbose\n\
\x20   --[no-]strict         enable strict checking\n\
\n\
";

/// Ref-name prefixes whose refs live in the per-worktree `$GIT_DIR` rather than
/// in the shared `$GIT_COMMON_DIR`, per git's `is_per_worktree_ref()`.
const PER_WORKTREE: [&str; 3] = ["refs/worktree/", "refs/bisect/", "refs/rewritten/"];

/// `git refs` — see the module docs for the covered surface.
pub fn refs(args: &[String]) -> Result<ExitCode> {
    // Dispatch strips the verb, so `args[0]` is this command's own subcommand.
    let Some(sub) = args.first() else {
        eprint!("error: need a subcommand\n{USAGE}");
        return Ok(ExitCode::from(129));
    };

    match sub.as_str() {
        // `parse_options_step()` answers `--help-all` with a `strcmp()` of its
        // own, ahead of `parse_long_opt()`, so the name never abbreviates and
        // never takes an `=<value>` — `--help-a` and `--help-all=x` stay
        // unknown options below. It renders `USAGE_FULL`, which is this same
        // block: the table has no `PARSE_OPT_HIDDEN` entry to reveal.
        "-h" | "--help-all" => {
            print!("{USAGE}");
            Ok(ExitCode::from(129))
        }
        "exists" => exists(&args[1..]),
        "list" => list(&args[1..]),
        // `optimize` keeps its own leading token: the pack-refs port skips it.
        "optimize" => optimize(args),
        "migrate" => migrate(&args[1..]),
        "verify" => verify(&args[1..]),
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
            // `--help-all` is a `strcmp()` inside `parse_options_step()`'s own
            // loop, ahead of `parse_long_opt()`: never abbreviated, never
            // `=<value>`, and never seen past the `--` handled above. Its
            // `USAGE_FULL` is this block — the table has no hidden entry.
            if a == "-h" || a == "--help-all" {
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

/// `git refs verify` — check the reference database for consistency.
///
/// `cmd_refs_verify()` reads `fsck.<msg-id>` and `fsck.skipList` through
/// `git_fsck_config()`, then runs `refs_fsck()` over every worktree. The checks
/// themselves live in [`super::fsck::fsck_refs`], which `git fsck` also reaches
/// for `--references` — git spawns this very command there.
///
/// git returns `error()`'s `-1` from `cmd_refs()`, which the process truncates
/// to an exit status of 255; a run whose only findings were warnings exits 0.
fn verify(args: &[String]) -> Result<ExitCode> {
    let mut verbose = false;
    let mut strict = false;

    for a in args {
        match a.as_str() {
            // `--help-all`'s own `strcmp()` in `parse_options_step()` runs
            // before `parse_long_opt()`, so it never abbreviates and never
            // takes a value; `USAGE_FULL` equals this block because the table
            // has no `PARSE_OPT_HIDDEN` entry.
            "-h" | "--help-all" => {
                print!("{USAGE_VERIFY}");
                return Ok(ExitCode::from(129));
            }
            "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            "--strict" => strict = true,
            "--no-strict" => strict = false,
            s if s.starts_with("--") => {
                eprintln!("error: unknown option `{}'", &s[2..]);
                eprint!("{USAGE_VERIFY}");
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with('-') && s.len() > 1 => {
                eprintln!("error: unknown switch `{}'", &s[1..2]);
                eprint!("{USAGE_VERIFY}");
                return Ok(ExitCode::from(129));
            }
            // `usage()`, not `die()`: no `fatal:` prefix and exit 129.
            _ => {
                eprintln!("usage: 'git refs verify' takes no arguments");
                return Ok(ExitCode::from(129));
            }
        }
    }

    let repo = gix::discover(".")?;
    let config = match super::fsck::MsgConfig::new(&repo, super::fsck::MsgSource::Fsck { strict }) {
        Ok(config) => config,
        // `git_fsck_config()` dies before any checking starts.
        Err(fatal) => {
            eprintln!("fatal: {fatal}");
            return Ok(ExitCode::from(128));
        }
    };

    if super::fsck::fsck_refs(&repo, &config, verbose) {
        return Ok(ExitCode::from(255));
    }
    Ok(ExitCode::SUCCESS)
}

/// `git refs migrate --ref-format=<format> [--no-reflog] [--dry-run]`.
///
/// `cmd_refs_migrate()` (builtin/refs.c) makes four decisions before it moves a byte, and
/// all four are reproduced here:
///
///  1. leftover positionals — `usage(_("too many arguments"))`;
///  2. no `--ref-format` — `usage(_("missing --ref-format=<format>"))`;
///  3. a name `ref_storage_format_by_name()` does not know — `error(_("unknown ref storage
///     format '%s'"))`;
///  4. the repository already in that format — `error(_("repository already uses '%s'
///     format"))`.
///
/// `usage()` exits 129. The `error()` paths return `-1` up through `cmd_refs()`, which the
/// process truncates to 255 — not 1, which is what an `error()` returning `1` would give.
///
/// Only step 5, `repo_migrate_ref_storage_format()`, is out of reach: it writes the target
/// backend, and the vendored `gix-ref` has no reftable implementation to write.
fn migrate(args: &[String]) -> Result<ExitCode> {
    let mut format_str: Option<String> = None;
    let mut positionals: Vec<&str> = Vec::new();
    let mut end_of_opts = false;
    let mut i = 0usize;

    while i < args.len() {
        let a = args[i].as_str();
        i += 1;
        if end_of_opts || a == "-" || !a.starts_with('-') {
            positionals.push(a);
            continue;
        }
        if a == "--" {
            end_of_opts = true;
            continue;
        }
        match a {
            // Same `strcmp()` in `parse_options_step()`, ahead of
            // `parse_long_opt()` and after the `--` break above: exact name
            // only, and `USAGE_FULL` is this block, there being no hidden
            // entry in the table.
            "-h" | "--help-all" => {
                print!("{USAGE_MIGRATE}");
                return Ok(ExitCode::from(129));
            }
            "--dry-run" | "--no-dry-run" | "--no-reflog" | "--reflog" => {}
            // `PARSE_OPT_NONEG`, so there is no `--no-ref-format`; that name falls
            // through to the unknown-option arm below exactly as git's does.
            "--ref-format" => match args.get(i) {
                Some(v) => {
                    format_str = Some(v.clone());
                    i += 1;
                }
                // `parse-options` prints this one without the usage block.
                None => {
                    eprintln!("error: option `ref-format' requires a value");
                    return Ok(ExitCode::from(129));
                }
            },
            _ => {
                if let Some(v) = a.strip_prefix("--ref-format=") {
                    format_str = Some(v.to_string());
                } else if let Some(long) = a.strip_prefix("--") {
                    eprintln!("error: unknown option `{long}'");
                    eprint!("{USAGE_MIGRATE}");
                    return Ok(ExitCode::from(129));
                } else {
                    eprintln!("error: unknown switch `{}'", &a[1..2]);
                    eprint!("{USAGE_MIGRATE}");
                    return Ok(ExitCode::from(129));
                }
            }
        }
    }

    // `usage()`: the message alone on stderr, no `fatal:` prefix, no usage block.
    if !positionals.is_empty() {
        eprintln!("usage: too many arguments");
        return Ok(ExitCode::from(129));
    }
    let Some(format_str) = format_str else {
        eprintln!("usage: missing --ref-format=<format>");
        return Ok(ExitCode::from(129));
    };

    if !REF_FORMATS.contains(&format_str.as_str()) {
        eprintln!("error: unknown ref storage format '{format_str}'");
        return Ok(ExitCode::from(255));
    }

    let repo = gix::discover(".")?;
    if current_ref_format(&repo) == format_str {
        eprintln!("error: repository already uses '{format_str}' format");
        return Ok(ExitCode::from(255));
    }

    bail!(
        "refs migrate: cannot convert to '{format_str}': the vendored gix-ref implements only \
         the loose+packed files backend, so there is no reftable writer to migrate into"
    )
}

/// The repository's reference backend, as `repo_settings`'s `ref_storage_format` resolves it:
/// the `extensions.refStorage` value, or `files` when the extension is absent.
fn current_ref_format(repo: &gix::Repository) -> String {
    repo.config_snapshot()
        .string("extensions.refStorage")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "files".to_string())
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
            // `--help-all` reaches `usage_with_options_internal()` through its
            // own `strcmp()` in `parse_options_step()`, before any long-option
            // resolution, so no prefix of it and no `=<value>` form counts. The
            // block is the same one `-h` prints: no hidden entry to add.
            } else if a == "-h" || a == "--help-all" {
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
    // `--help-all` is `parse_options_step()`'s own `strcmp()`, placed after the
    // `--` break and before `parse_long_opt()`: the exact name only (no prefix,
    // no `=<value>`) and never past a `--`, which is why this scan stops there.
    // `USAGE_FULL` is the same block, pack-refs' table having no hidden entry.
    if args[1..].iter().take_while(|a| a.as_str() != "--").any(|a| a == "--help-all") {
        print!("{USAGE_OPTIMIZE}");
        return Ok(ExitCode::from(129));
    }
    if args[1..].iter().any(|a| a == "-h") {
        print!("{USAGE_OPTIMIZE}");
        return Ok(ExitCode::from(129));
    }
    super::pack_refs::pack_refs(args)
}
