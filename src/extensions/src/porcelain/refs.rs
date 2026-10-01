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
//!   * the subcommand dispatch itself: `-h` (usage on stdout, exit 0), a
//!     missing subcommand (`error: need a subcommand` + usage on stderr, 129),
//!     an unknown subcommand, and each subcommand's own `-h` usage block.
//!
//!   * `git refs create`/`delete`/`update`/`rename` (new in 2.56): the first
//!     three share `update-ref`'s command-line write
//!     ([`super::update_ref::write_cmdline`]), reporting a refused write as
//!     `error:` and exit 1 where `update-ref` dies; `rename` is
//!     `files_copy_or_rename_ref()`, which moves the ref and its reflog only.
//!   * `git refs migrate --ref-format=<format>` up to the point where bytes would
//!     move: the option scan, `usage: too many arguments`, `usage: missing
//!     --ref-format=<format>`, `error: unknown ref storage format '<x>'`,
//!     `error: repository already uses '<x>' format`, and — inside
//!     `repo_migrate_ref_storage_format()` itself, ahead of either backend —
//!     `error: migrating repositories with worktrees is not supported yet`
//!     (refs.c:3366) at exit 255. Every one of them is a decision about names,
//!     the repository's current format, or its worktrees.
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
//!
//! Known divergence, and the larger half of the same gap: a repository that
//! *declares* `extensions.refStorage = reftable` at
//! `core.repositoryFormatVersion = 1` is read here — and by `show-ref`,
//! `symbolic-ref`, `update-ref`, `pack-refs` and `reflog` — as though the
//! declaration were absent, so the loose and packed files under `refs/` are
//! served as the ref store. [`current_ref_format`] is the only place in this
//! cluster that consults the declaration at all, and it only *reports* it.
//!
//! Stock git believes the declaration: it opens a reftable store, and over a
//! files repository (the state a half-finished migration leaves behind) that
//! store is empty. Measured against git 2.55.0 on such a repository —
//! `show-ref` exits 1 printing nothing, `symbolic-ref HEAD` dies with `ref HEAD
//! is not a symbolic ref` at 128, `pack-refs --all` exits 254 and
//! `symbolic-ref HEAD refs/heads/<b>` fails the write with `reftable:
//! transaction prepare: I/O error`. This port answers all four from the files
//! backend and exits 0, which is the dangerous direction: a caller cannot tell
//! "these are the refs" from "these are the refs of a store git is not using".
//!
//! The fix is not per-command. A reader may only serve the files backend when
//! the repository declares it, so the decision belongs with the config read that
//! already refuses a backend *value* this build does not know
//! ([`crate::config::extension_value_refusal`], which is what turns
//! `extensions.refStorage = bogusbackend` into `invalid value for
//! 'extensions.refstorage'`), not repeated in each verb — a gate added to some
//! verbs and not others would make `show-ref` and `for-each-ref` disagree about
//! the same repository. Honouring the declaration rather than refusing it needs
//! a reftable backend `gix-ref` does not have.

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
\x20  or: git refs create [--message=<reason>] [--no-deref] [--create-reflog] <ref> <new-value>\n\
\x20  or: git refs delete [--message=<reason>] [--no-deref] <ref> [<old-value>]\n\
\x20  or: git refs update [--message=<reason>] [--no-deref] [--create-reflog] <ref> <new-value> [<old-value>]\n\
\x20  or: git refs rename [--message=<reason>] <old-ref> <new-ref>\n\
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

/// `cmd_refs_verify`'s `struct option options[]` (builtin/refs.c), in table order:
/// two `OPT_BOOL`s, so both negate and neither takes a value.
const VERIFY_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "verbose", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "strict",  neg: true, arg: super::Arg::None },
];

/// `cmd_refs_migrate`'s `struct option options[]` (builtin/refs.c), in table order.
///
/// `ref-format` is `OPT_STRING_F(… PARSE_OPT_NONEG)`, so it takes a value and has no
/// `--no-` spelling; the other two are `OPT_BIT`s, which take none and negate. Note
/// that `no-reflog` is the *entry's own* name, so parse-options reads `--reflog` as
/// its unset sense — which is why the two spellings are one table row rather than two.
const MIGRATE_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "ref-format", neg: false, arg: super::Arg::Required },
    super::LongOpt { name: "dry-run",    neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "no-reflog",  neg: true,  arg: super::Arg::None },
];

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
            Ok(super::show_usage(USAGE))
        }
        "exists" => exists(&args[1..]),
        "list" => list(&args[1..]),
        // `optimize` keeps its own leading token: the pack-refs port skips it.
        "optimize" => optimize(args),
        "migrate" => migrate(&args[1..]),
        "verify" => verify(&args[1..]),
        "create" => create(&args[1..]),
        "delete" => delete(&args[1..]),
        "update" => update(&args[1..]),
        "rename" => rename(&args[1..]),
        // git's option parser reports an unknown leading dashed argument before
        // it ever looks for a subcommand.
        // `parse_options_step()` consumes a lone `--` before any table lookup
        // (parse-options.c: `if (!arg[2]) { ... ctx->argc--; ctx->argv++; break; }`),
        // so it is never an unknown option. What is left is a command line with
        // no sub-command word in it, which is `PARSE_OPT_SUBCOMMAND`'s own
        // refusal — the same one an empty argv gets.
        "--" => {
            eprint!("error: need a subcommand\n{USAGE}");
            Ok(ExitCode::from(129))
        }
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
                return Ok(super::show_usage(USAGE_EXISTS));
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

    let repo = crate::setup::discover()?;
    Ok(exit_for_raw_ref(read_raw_ref(&repo, name)))
}

/// What `refs_read_raw_ref()` made of a name, in the three shapes its two
/// callers — `cmd_refs_exists()` here and `cmd_show_ref__exists()` in
/// [`super::show_ref`] — split its `failure_errno` into.
pub(super) enum RawRef {
    /// It returned 0: something is there, whatever it points at.
    Present,
    /// It failed with `ENOENT` or `EISDIR` — no loose file (or a directory
    /// where one would be) and no packed entry either.
    Missing,
    /// It failed with any other `errno`, carried here so `strerror` can render
    /// it. The files backend reaches this only through
    /// `parse_loose_ref_contents()`'s `EINVAL`: a loose file whose contents are
    /// neither `ref: <name>` nor an object id.
    Unreadable(i32),
}

/// The exit code and `error()` line the two `exists` commands share, which are
/// character-for-character the same C:
///
/// ```c
/// if (failure_errno == ENOENT || failure_errno == EISDIR) {
///         error(_("reference does not exist"));
///         ret = 2;
/// } else {
///         errno = failure_errno;
///         error_errno(_("failed to look up reference"));
///         ret = 1;
/// }
/// ```
///
/// The distinction is the whole point of the exit code: 2 means the ref-store
/// answered "no such ref", 1 means it could not answer at all, and a port that
/// collapses the second into the first reports a corrupt loose ref as an absent
/// one.
pub(super) fn exit_for_raw_ref(result: RawRef) -> ExitCode {
    match result {
        RawRef::Present => ExitCode::SUCCESS,
        RawRef::Missing => {
            eprintln!("error: reference does not exist");
            ExitCode::from(2)
        }
        RawRef::Unreadable(errno) => {
            // `error_errno()` appends `: strerror(errno)`.
            let err = std::io::Error::from_raw_os_error(errno);
            eprintln!("error: failed to look up reference: {}", crate::external::strerror(&err));
            ExitCode::from(1)
        }
    }
}

/// `refs_read_raw_ref()` (refs.c) against the files backend, looked up
/// *verbatim*.
///
/// There is no rev-parse DWIM here, so `master` does not find
/// `refs/heads/master` — hence this bypasses `gix`'s partial-name search (which
/// walks `refs/`, `refs/tags/`, `refs/heads/`, `refs/remotes/`) and reads the one
/// loose path the name maps to, falling back to `packed-refs`.
///
/// `files_read_raw_ref()` (refs/files-backend.c) resolves in that order and reads
/// the file rather than merely testing for it:
///
/// ```c
/// stat_ref:
///         if (lstat(path, &st) < 0) {  /* ENOENT → try packed, else that errno */ }
///         if (S_ISDIR(st.st_mode)) {   /* try packed, else EISDIR */ }
///         …
///         strbuf_rtrim(&sb_contents);
///         ret = parse_loose_ref_contents(…, &myerr);
/// ```
///
/// so a loose file holding something that is neither `ref: <name>` nor an object
/// id is a *read failure*, not an absence — the difference
/// [`exit_for_raw_ref`] turns into 1 rather than 2.
///
/// A name that is not a valid full ref name is reported as missing, which is what
/// git does for e.g. `refs/heads/../x`; it also keeps the name from being joined
/// onto a path it could escape.
pub(super) fn read_raw_ref(repo: &gix::Repository, name: &str) -> RawRef {
    if gix::refs::FullName::try_from(name).is_err() {
        return RawRef::Missing;
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
    let path = base.join(name);
    if path.is_file() {
        return match std::fs::read(&path) {
            Ok(contents) => match loose_contents_parse(&contents, repo.object_hash()) {
                true => RawRef::Present,
                false => RawRef::Unreadable(libc::EINVAL),
            },
            Err(err) => RawRef::Unreadable(err.raw_os_error().unwrap_or(libc::EIO)),
        };
    }

    // Only `refs/`-rooted names can ever appear in `packed-refs`. A directory
    // where the loose ref would be reaches the packed store the same way a
    // missing file does, and is `EISDIR` — which is a `Missing` either way.
    if !name.starts_with("refs/") {
        return RawRef::Missing;
    }
    match store.open_packed_buffer() {
        Ok(Some(packed)) => match packed.try_find(name) {
            Ok(Some(_)) => RawRef::Present,
            Ok(None) => RawRef::Missing,
            Err(_) => RawRef::Unreadable(libc::EINVAL),
        },
        Ok(None) => RawRef::Missing,
        // `packed_refs_lock()`/`get_packed_ref_cache()` die on a `packed-refs`
        // they cannot map or parse; the errno that reaches the caller is not
        // `ENOENT`, so it lands on the same side of the split as `EINVAL`.
        Err(_) => RawRef::Unreadable(libc::EINVAL),
    }
}

/// `parse_loose_ref_contents()` (refs/files-backend.c), reduced to the one bit
/// its `exists` callers read: whether it succeeded.
///
/// ```c
/// if (skip_prefix(buf, "ref:", &buf)) { … return 0; }
/// if (parse_oid_hex_algop(buf, oid, &p, algop) ||
///     (*p != '\0' && !isspace(*p))) {
///         *type |= REF_ISBROKEN;
///         *failure_errno = EINVAL;
///         return -1;
/// }
/// ```
///
/// A `ref:` line is accepted whatever follows it — a dangling symref exists — and
/// anything else has to open with a full-width object id that ends the token, the
/// trailing-data allowance being what lets `FETCH_HEAD` parse. The caller has
/// already applied `strbuf_rtrim()`, which is the `trim_ascii_end` here.
fn loose_contents_parse(contents: &[u8], hash: gix::hash::Kind) -> bool {
    let buf = trim_ascii_end(contents);
    if buf.starts_with(b"ref:") {
        return true;
    }
    let width = hash.len_in_hex();
    buf.len() >= width
        && buf[..width].iter().all(u8::is_ascii_hexdigit)
        && buf[width..]
            .first()
            .is_none_or(|c| c.is_ascii_whitespace())
}

/// `strbuf_rtrim()`: git trims the whole `isspace` set off the end before it
/// parses, so a loose ref written with a trailing `\r\n` still reads.
fn trim_ascii_end(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[..end]
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

    // `parse_options_step()` ends the option scan at `--` and at
    // `--end-of-options`, dropping the token itself; everything after is a
    // positional, and `cmd_refs_verify()` answers any positional with
    // `usage(_("'git refs verify' takes no arguments"))`.
    let mut end_of_opts = false;
    for a in args {
        let a = a.as_str();
        if end_of_opts {
            eprintln!("usage: 'git refs verify' takes no arguments");
            return Ok(ExitCode::from(129));
        }
        match a {
            "--" | "--end-of-options" => end_of_opts = true,
            // `--help-all`'s own `strcmp()` in `parse_options_step()` runs
            // before `parse_long_opt()`, so it never abbreviates and never
            // takes a value; `USAGE_FULL` equals this block because the table
            // has no `PARSE_OPT_HIDDEN` entry.
            "-h" | "--help-all" => {
                return Ok(super::show_usage(USAGE_VERIFY));
            }
            _ if a.starts_with("--") => {
                let body = &a[2..];
                let (opt, unset) = match super::resolve_long(VERIFY_OPTS, body) {
                    super::Resolved::One(opt, unset) => (opt, unset),
                    super::Resolved::Ambiguous(first, second) => {
                        return Ok(super::ambiguous_option(a, &first, &second, USAGE_VERIFY))
                    }
                    super::Resolved::Unknown => {
                        eprintln!("error: unknown option `{body}'");
                        eprint!("{USAGE_VERIFY}");
                        return Ok(ExitCode::from(129));
                    }
                };
                // Both entries are `OPT_BOOL`, so an attached value is
                // `PARSE_OPT_ERROR` out of `get_value()`: its own line, no block.
                if body.contains('=') {
                    let shown = match unset {
                        true => format!("no-{}", opt.name),
                        false => opt.name.to_string(),
                    };
                    eprintln!("error: option `{shown}' takes no value");
                    return Ok(ExitCode::from(129));
                }
                match opt.name {
                    "verbose" => verbose = !unset,
                    "strict" => strict = !unset,
                    _ => unreachable!("resolve_long only returns VERIFY_OPTS entries"),
                }
            }
            _ if a.starts_with('-') && a.len() > 1 => {
                eprintln!("error: unknown switch `{}'", &a[1..2]);
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

    let repo = crate::setup::discover()?;
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
                return Ok(super::show_usage(USAGE_MIGRATE));
            }
            _ if a.starts_with("--") => {
                let body = &a[2..];
                // `parse_long_opt()` resolves the whole body, `=<value>` included.
                let inline = body.split_once('=').map(|(_, v)| v);
                let (opt, unset) = match super::resolve_long(MIGRATE_OPTS, body) {
                    super::Resolved::One(opt, unset) => (opt, unset),
                    super::Resolved::Ambiguous(first, second) => {
                        return Ok(super::ambiguous_option(a, &first, &second, USAGE_MIGRATE))
                    }
                    super::Resolved::Unknown => {
                        eprintln!("error: unknown option `{body}'");
                        eprint!("{USAGE_MIGRATE}");
                        return Ok(ExitCode::from(129));
                    }
                };
                let shown = match unset {
                    true => format!("no-{}", opt.name),
                    false => opt.name.to_string(),
                };
                if opt.arg == super::Arg::Required && !unset {
                    // `get_value()`: the attached value, else the next argument.
                    match inline {
                        Some(v) => format_str = Some(v.to_string()),
                        None => match args.get(i) {
                            Some(v) => {
                                format_str = Some(v.clone());
                                i += 1;
                            }
                            // `parse-options` prints this one without the usage block.
                            None => {
                                eprintln!("error: option `{shown}' requires a value");
                                return Ok(ExitCode::from(129));
                            }
                        },
                    }
                } else if inline.is_some() {
                    eprintln!("error: option `{shown}' takes no value");
                    return Ok(ExitCode::from(129));
                }
                // `dry-run` and `no-reflog` only set bits this port never reaches:
                // the migration itself is refused below, before either could matter.
            }
            _ => {
                eprintln!("error: unknown switch `{}'", &a[1..2]);
                eprint!("{USAGE_MIGRATE}");
                return Ok(ExitCode::from(129));
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

    let repo = crate::setup::discover()?;
    if current_ref_format(&repo) == format_str {
        eprintln!("error: repository already uses '{format_str}' format");
        return Ok(ExitCode::from(255));
    }

    // `repo_migrate_ref_storage_format()` (refs.c:3366) refuses a repository with a
    // linked worktree before it touches either backend:
    //
    // ```c
    // if (has_worktrees()) {
    //         strbuf_addstr(errbuf, "migrating repositories with worktrees is not supported yet");
    //         ret = -1;
    //         goto done;
    // }
    // ```
    //
    // `cmd_refs_migrate()` reports that through `error("%s", errbuf.buf)` and returns
    // its `-1`, so the exit code is 255. `has_worktrees()` (refs.c:3314) counts every
    // worktree that is not the main one, which is what `repo.worktrees()` lists.
    if !repo.worktrees().map(|w| w.is_empty()).unwrap_or(true) {
        eprintln!("error: migrating repositories with worktrees is not supported yet");
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
                return Ok(super::show_usage(USAGE_LIST));
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
        return Ok(super::show_usage(USAGE_OPTIMIZE));
    }
    if args[1..].iter().any(|a| a == "-h") {
        return Ok(super::show_usage(USAGE_OPTIMIZE));
    }
    super::pack_refs::pack_refs(args)
}

/// `git refs create -h`, byte-for-byte.
const USAGE_CREATE: &str = "\
usage: git refs create [--message=<reason>] [--no-deref] [--create-reflog] <ref> <new-value>\n\
\n\
\x20   --[no-]message <reason>\n\
\x20                         reason of the update\n\
\x20   --no-deref            update <refname> not the one it points to\n\
\x20   --deref               opposite of --no-deref\n\
\x20   --[no-]create-reflog  create a reflog\n\
\n\
";

/// `git refs delete -h`, byte-for-byte.
const USAGE_DELETE: &str = "\
usage: git refs delete [--message=<reason>] [--no-deref] <ref> [<old-value>]\n\
\n\
\x20   --[no-]message <reason>\n\
\x20                         reason of the update\n\
\x20   --no-deref            update <refname> not the one it points to\n\
\x20   --deref               opposite of --no-deref\n\
\n\
";

/// `git refs update -h`, byte-for-byte.
const USAGE_UPDATE: &str = "\
usage: git refs update [--message=<reason>] [--no-deref] [--create-reflog] <ref> <new-value> [<old-value>]\n\
\n\
\x20   --[no-]message <reason>\n\
\x20                         reason of the update\n\
\x20   --no-deref            update <refname> not the one it points to\n\
\x20   --deref               opposite of --no-deref\n\
\x20   --[no-]create-reflog  create a reflog\n\
\n\
";

/// `git refs rename -h`, byte-for-byte.
const USAGE_RENAME: &str = "\
usage: git refs rename [--message=<reason>] <old-ref> <new-ref>\n\
\n\
\x20   --[no-]message <reason>\n\
\x20                         reason of the update\n\
\n\
";

/// `cmd_refs_create()`'s and `cmd_refs_update()`'s `struct option opts[]`
/// (builtin/refs.c:200-207, :291-298, v2.56.0): `OPT_STRING` `message`, then two
/// `OPT_BIT`s. `no-deref` carries its negation in its own name, so `--deref` is
/// its unset sense.
const CREATE_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "message",       neg: true, arg: super::Arg::Required },
    super::LongOpt { name: "no-deref",      neg: true, arg: super::Arg::None },
    super::LongOpt { name: "create-reflog", neg: true, arg: super::Arg::None },
];

/// `cmd_refs_delete()`'s table (builtin/refs.c:244-250): no `--create-reflog`.
const DELETE_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "message",  neg: true, arg: super::Arg::Required },
    super::LongOpt { name: "no-deref", neg: true, arg: super::Arg::None },
];

/// `cmd_refs_rename()`'s table (builtin/refs.c:339-343): `--message` alone.
const RENAME_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "message", neg: true, arg: super::Arg::Required },
];

/// What the four writing subcommands' `parse_options()` leaves behind.
#[derive(Default)]
struct WriteOpts {
    message: Option<String>,
    no_deref: bool,
    create_reflog: bool,
    args: Vec<String>,
}

/// `parse_options(argc, argv, prefix, opts, usage, 0)` over one of the tables
/// above: options and operands may interleave, `--` ends the options, `-h` and
/// `--help-all` print `usage` on stdout. There are no short options, so every
/// other `-<c>` is an unknown switch.
fn parse_write_opts(
    args: &[String],
    table: &'static [super::LongOpt],
    usage: &str,
) -> std::result::Result<WriteOpts, ExitCode> {
    let mut o = WriteOpts::default();
    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;
        if a == "--" {
            o.args.extend(args[i..].iter().cloned());
            break;
        }
        if a == "-" || !a.starts_with('-') {
            o.args.push(a.to_string());
            continue;
        }
        if a == "--help-all" || a.starts_with("-h") {
            return Err(super::show_usage(usage));
        }
        let Some(body) = a.strip_prefix("--") else {
            eprintln!("error: unknown switch `{}'", &a[1..2]);
            eprint!("{usage}");
            return Err(ExitCode::from(129));
        };
        let inline = body.split_once('=').map(|(_, v)| v);
        let (opt, unset) = match super::resolve_long(table, body) {
            super::Resolved::One(opt, unset) => (opt, unset),
            super::Resolved::Ambiguous(first, second) => {
                return Err(super::ambiguous_option(a, &first, &second, usage))
            }
            super::Resolved::Unknown => {
                eprintln!("error: unknown option `{body}'");
                eprint!("{usage}");
                return Err(ExitCode::from(129));
            }
        };
        let shown = match unset {
            true => format!("no-{}", opt.name),
            false => opt.name.to_string(),
        };
        if inline.is_some() && (unset || opt.arg == super::Arg::None) {
            eprintln!("error: option `{shown}' takes no value");
            return Err(ExitCode::from(129));
        }
        match opt.name {
            "message" if unset => o.message = None,
            "message" => {
                let value = match inline {
                    Some(v) => v.to_string(),
                    None => match args.get(i) {
                        Some(v) => {
                            i += 1;
                            v.clone()
                        }
                        None => {
                            eprintln!("error: option `{shown}' requires a value");
                            return Err(ExitCode::from(129));
                        }
                    },
                };
                o.message = Some(value);
            }
            // `OPT_BIT(0, "no-deref", …)`: the plain spelling sets the bit, the
            // `--deref` spelling (its unset sense) clears it.
            "no-deref" => o.no_deref = !unset,
            "create-reflog" => o.create_reflog = !unset,
            _ => unreachable!("every table entry is matched above"),
        }
    }
    Ok(o)
}

/// The steps every writing subcommand takes once its operands are counted:
///
/// ```c
/// if (message && !*message)
///         die(_("refusing to perform update with empty message"));
///
/// repo_config(repo, git_default_config, NULL);
/// ```
///
/// The repository is opened here, after the count, because `RUN_SETUP` has
/// already happened in git by then and an operand-count error must not depend
/// on it. Every write leaves a reflog line, which is why the identity a bare
/// runner lacks is synthesized, as `update-ref` does.
fn open_for_write(o: &WriteOpts) -> Result<std::result::Result<gix::Repository, ExitCode>> {
    if o.message.as_deref() == Some("") {
        eprintln!("fatal: refusing to perform update with empty message");
        return Ok(Err(ExitCode::from(128)));
    }
    let mut repo = crate::setup::discover()?;
    crate::ensure_reflog_identity(&mut repo);
    Ok(Ok(repo))
}

/// `repo_get_oid_with_flags(repo, spec, &oid, GET_OID_SKIP_AMBIGUITY_CHECK)`,
/// shared with `update-ref`'s value slots: the value `spec` names, or `None`
/// when it names nothing. An empty `spec` names nothing — `update-ref` alone
/// reads `""` as the null id, before it ever calls this.
fn get_oid(repo: &gix::Repository, spec: &str) -> Option<super::update_ref::Val> {
    if spec.is_empty() {
        return None;
    }
    super::update_ref::resolve_slot(repo, spec)
}

/// `git refs create [--message=<reason>] [--no-deref] [--create-reflog] <ref> <new-value>`
/// — `cmd_refs_create()` (builtin/refs.c:190-233, v2.56.0).
///
/// `refs_update_ref()` with a null old value, so the reference must not exist
/// yet, and `UPDATE_REFS_MSG_ON_ERR`, so a refused write is `error:` and exit 1.
fn create(args: &[String]) -> Result<ExitCode> {
    use super::update_ref::Val;
    let o = match parse_write_opts(args, CREATE_OPTS, USAGE_CREATE) {
        Ok(o) => o,
        Err(code) => return Ok(code),
    };
    if o.args.len() != 2 {
        eprintln!("usage: create requires reference name and an object ID");
        return Ok(ExitCode::from(129));
    }
    let repo = match open_for_write(&o)? {
        Ok(repo) => repo,
        Err(code) => return Ok(code),
    };
    let (name, spec) = (o.args[0].as_str(), o.args[1].as_str());
    let new = match get_oid(&repo, spec) {
        Some(Val::Zero) => {
            eprintln!("fatal: cannot create reference with null new object ID");
            return Ok(ExitCode::from(128));
        }
        Some(v) => v,
        None => {
            eprintln!("fatal: invalid object ID: '{spec}'");
            return Ok(ExitCode::from(128));
        }
    };
    let write = super::update_ref::CmdlineWrite {
        name,
        new: &new,
        old: &Val::Zero,
        delete: false,
        deref: !o.no_deref,
        create_reflog: o.create_reflog,
        msg: o.message.as_deref(),
    };
    super::update_ref::write_cmdline(&repo, &write, super::update_ref::OnErr::Msg)
}

/// `git refs delete [--message=<reason>] [--no-deref] <ref> [<old-value>]` —
/// `cmd_refs_delete()` (builtin/refs.c:236-280, v2.56.0), which is
/// `refs_delete_ref()` exactly as `update-ref -d` calls it.
fn delete(args: &[String]) -> Result<ExitCode> {
    use super::update_ref::Val;
    let o = match parse_write_opts(args, DELETE_OPTS, USAGE_DELETE) {
        Ok(o) => o,
        Err(code) => return Ok(code),
    };
    if o.args.is_empty() || o.args.len() > 2 {
        eprintln!("usage: delete requires reference name and an optional old object ID");
        return Ok(ExitCode::from(129));
    }
    let repo = match open_for_write(&o)? {
        Ok(repo) => repo,
        Err(code) => return Ok(code),
    };
    let name = o.args[0].as_str();
    let old = match o.args.get(1) {
        None => Val::Missing,
        Some(spec) => match get_oid(&repo, spec) {
            Some(Val::Zero) => {
                eprintln!("fatal: cannot delete reference with null old object ID");
                return Ok(ExitCode::from(128));
            }
            Some(v) => v,
            None => {
                eprintln!("fatal: invalid old object ID: '{spec}'");
                return Ok(ExitCode::from(128));
            }
        },
    };
    let write = super::update_ref::CmdlineWrite {
        name,
        new: &Val::Missing,
        old: &old,
        delete: true,
        deref: !o.no_deref,
        create_reflog: false,
        msg: o.message.as_deref(),
    };
    super::update_ref::write_cmdline(&repo, &write, super::update_ref::OnErr::Msg)
}

/// `git refs update [--message=<reason>] [--no-deref] [--create-reflog] <ref>
/// <new-value> [<old-value>]` — `cmd_refs_update()` (builtin/refs.c:283-330,
/// v2.56.0). A null `<new-value>` deletes; a null `<old-value>` demands that the
/// reference not exist yet.
fn update(args: &[String]) -> Result<ExitCode> {
    use super::update_ref::Val;
    let o = match parse_write_opts(args, CREATE_OPTS, USAGE_UPDATE) {
        Ok(o) => o,
        Err(code) => return Ok(code),
    };
    if o.args.len() < 2 || o.args.len() > 3 {
        eprintln!("usage: update requires reference name, new value and an optional old value");
        return Ok(ExitCode::from(129));
    }
    let repo = match open_for_write(&o)? {
        Ok(repo) => repo,
        Err(code) => return Ok(code),
    };
    let name = o.args[0].as_str();
    let Some(new) = get_oid(&repo, &o.args[1]) else {
        eprintln!("fatal: invalid new object ID: '{}'", o.args[1]);
        return Ok(ExitCode::from(128));
    };
    let old = match o.args.get(2) {
        None => Val::Missing,
        Some(spec) => match get_oid(&repo, spec) {
            Some(v) => v,
            None => {
                eprintln!("fatal: invalid old object ID: '{spec}'");
                return Ok(ExitCode::from(128));
            }
        },
    };
    let write = super::update_ref::CmdlineWrite {
        name,
        new: &new,
        old: &old,
        delete: false,
        deref: !o.no_deref,
        create_reflog: o.create_reflog,
        msg: o.message.as_deref(),
    };
    super::update_ref::write_cmdline(&repo, &write, super::update_ref::OnErr::Msg)
}

/// `git refs rename [--message=<reason>] <old-ref> <new-ref>` —
/// `cmd_refs_rename()` (builtin/refs.c:333-375, v2.56.0):
///
/// ```c
/// if (check_refname_format(oldref, 0))
///         die(_("invalid ref format: '%s'"), oldref);
/// if (check_refname_format(newref, 0))
///         die(_("invalid ref format: '%s'"), newref);
///
/// if (!refs_ref_exists(get_main_ref_store(repo), oldref))
///         die(_("reference does not exist: '%s'"), oldref);
/// if (refs_ref_exists(get_main_ref_store(repo), newref))
///         die(_("reference already exists: '%s'"), newref);
///
/// ret = refs_rename_ref(get_main_ref_store(repo), oldref, newref, message);
/// ```
///
/// Only the reference and its reflog move: unlike `git branch -m`, nothing
/// re-points a `HEAD` that named the old reference, and no config follows it.
fn rename(args: &[String]) -> Result<ExitCode> {
    let o = match parse_write_opts(args, RENAME_OPTS, USAGE_RENAME) {
        Ok(o) => o,
        Err(code) => return Ok(code),
    };
    if o.args.len() != 2 {
        eprintln!("usage: rename requires old and new reference name");
        return Ok(ExitCode::from(129));
    }
    let repo = match open_for_write(&o)? {
        Ok(repo) => repo,
        Err(code) => return Ok(code),
    };
    let (old, new) = (o.args[0].as_str(), o.args[1].as_str());
    for name in [old, new] {
        if !super::check_ref_format::check_refname_format(name.as_bytes(), 0) {
            eprintln!("fatal: invalid ref format: '{name}'");
            return Ok(ExitCode::from(128));
        }
    }
    if !crate::refname::ref_exists(&repo, old.as_bytes()) {
        eprintln!("fatal: reference does not exist: '{old}'");
        return Ok(ExitCode::from(128));
    }
    if crate::refname::ref_exists(&repo, new.as_bytes()) {
        eprintln!("fatal: reference already exists: '{new}'");
        return Ok(ExitCode::from(128));
    }
    // `refs_rename_ref()` (refs.c:3126-3136) normalizes the message the way every
    // reflog write does.
    let msg = o.message.as_deref().map(super::reflog::normalize_reflog_message);
    match files_rename_ref(&repo, old, new, msg.as_deref().unwrap_or(""))? {
        true => Ok(ExitCode::SUCCESS),
        false => Ok(ExitCode::from(1)),
    }
}

/// `TMP_RENAMED_LOG` (refs/files-backend.c): where a rename parks the old
/// reference's reflog while the old reference is taken out of the way.
const TMP_RENAMED_LOG: &str = "refs/.tmp-renamed-log";

/// `files_ref_path()`/`files_reflog_path()`'s choice of directory: a
/// per-worktree reference lives in the worktree's own git directory, every
/// other one in the common directory.
fn files_base(repo: &gix::Repository, refname: &str) -> std::path::PathBuf {
    match PER_WORKTREE.iter().any(|p| refname.starts_with(p)) {
        true => repo.git_dir().to_path_buf(),
        false => repo.common_dir().to_path_buf(),
    }
}

/// `files_copy_or_rename_ref(…, copy = 0)` (refs/files-backend.c:1688-1840,
/// v2.56.0): the `rename_ref` method `refs_rename_ref()` dispatches to. Returns
/// `false` once it has reported the failure through `error()`, which
/// `cmd_refs_rename()` turns into exit 1.
///
/// The order is the C function's, and it is what lets `refs/heads/a` become
/// `refs/heads/a/b`: the old reflog is parked under [`TMP_RENAMED_LOG`] and the
/// old reference deleted (its `HEAD` mirror carrying `logmsg`) before anything
/// is written under the new name, then the parked log is moved into place and
/// the new reference written with a `<oid> <oid> … <logmsg>` entry.
fn files_rename_ref(repo: &gix::Repository, old: &str, new: &str, logmsg: &str) -> Result<bool> {
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    use gix::refs::{FullName, Target};

    let sb_oldref = files_base(repo, old).join("logs").join(old);
    let sb_newref = files_base(repo, new).join("logs").join(new);
    let tmp_renamed_log = repo.common_dir().join("logs").join(TMP_RENAMED_LOG);

    let loginfo = std::fs::symlink_metadata(&sb_oldref);
    let log = loginfo.is_ok();
    if loginfo.is_ok_and(|m| m.file_type().is_symlink()) {
        eprintln!("error: reflog for {old} is a symlink");
        return Ok(false);
    }

    // `refs_resolve_ref_unsafe(…, RESOLVE_REF_READING | RESOLVE_REF_NO_RECURSE, …)`.
    let Some(found) = repo.refs.try_find(old).ok().flatten().filter(|r| r.name.as_bstr() == old) else {
        eprintln!("error: refname {old} not found");
        return Ok(false);
    };
    let orig_oid = match found.target {
        Target::Object(id) => id,
        Target::Symbolic(_) => {
            eprintln!("error: refname {old} is a symbolic ref, renaming it is not supported");
            return Ok(false);
        }
    };
    // `refs_rename_ref_available()`: `refs_verify_refname_available(new, NULL,
    // skip = {old})`, reported through `error("%s", err.buf)`.
    if let Some(blocker) = refname_conflict(repo, new, old) {
        eprintln!("error: '{blocker}' exists; cannot create '{new}'");
        return Ok(false);
    }

    if log {
        if let Some(parent) = tmp_renamed_log.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::rename(&sb_oldref, &tmp_renamed_log) {
            eprintln!(
                "error: unable to move logfile logs/{old} to logs/{TMP_RENAMED_LOG}: {}",
                crate::external::strerror(&e)
            );
            return Ok(false);
        }
    }

    let full = |name: &str| -> Result<FullName> {
        name.try_into().map_err(|e| anyhow::anyhow!("invalid ref name '{name}': {e}"))
    };
    // `refs_delete_ref(&refs->base, logmsg, oldrefname, &orig_oid, REF_NO_DEREF)`.
    let deleted = repo.edit_reference(RefEdit {
        change: Change::Delete {
            expected: PreviousValue::MustExistAndMatch(Target::Object(orig_oid)),
            log: RefLog::AndReference,
            message: logmsg.into(),
        },
        name: full(old)?,
        deref: false,
    });
    if deleted.is_err() {
        eprintln!("error: unable to delete old {old}");
        return rename_rollback(repo, old, orig_oid, log, false, &sb_oldref, &sb_newref, &tmp_renamed_log);
    }

    // What stands at the new name: an empty directory tree left behind by
    // references that once lived below it, which `raceproof_create_file()`
    // clears (`remove_dir_recursively(…, REMOVE_DIR_EMPTY_ONLY)`) for both the
    // reference and its log. A real reference there was refused above.
    remove_empty_directories(&files_base(repo, new).join(new));
    remove_empty_directories(&sb_newref);

    // `rename_tmp_log()`.
    if log {
        if let Some(parent) = sb_newref.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::rename(&tmp_renamed_log, &sb_newref) {
            match e.raw_os_error() == Some(libc::EISDIR) {
                true => eprintln!("error: directory not empty: {}", sb_newref.display()),
                false => eprintln!(
                    "error: unable to move logfile {} to {}: {}",
                    tmp_renamed_log.display(),
                    sb_newref.display(),
                    crate::external::strerror(&e)
                ),
            }
            return rename_rollback(repo, old, orig_oid, log, false, &sb_oldref, &sb_newref, &tmp_renamed_log);
        }
    }

    // `lock_ref_oid_basic()` + `write_ref_to_lockfile()` + `commit_ref_update(…,
    // logmsg, 0)`, with `lock->old_oid` set to `orig_oid`: the entry reads
    // `<orig> <orig>`, written only where `log_ref_setup()` would — the log just
    // moved in, or a name `should_autocreate_reflog()` covers.
    let written = repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange { mode: RefLog::AndReference, force_create_reflog: false, message: logmsg.into() },
            expected: PreviousValue::MustNotExist,
            new: Target::Object(orig_oid),
        },
        name: full(new)?,
        deref: false,
    });
    if let Err(e) = written {
        eprintln!("error: unable to rename '{old}' to '{new}': {e}");
        return rename_rollback(repo, old, orig_oid, log, log, &sb_oldref, &sb_newref, &tmp_renamed_log);
    }
    // gitoxide logs the write as a creation; git's `lock->old_oid` makes it
    // `<orig> <orig>`.
    rewrite_last_reflog_old_id(&sb_newref, orig_oid);
    Ok(true)
}

/// The `rollback:` and `rollbacklog:` tail of `files_copy_or_rename_ref()`: put
/// the old reference back (`commit_ref_update(…, NULL, REF_SKIP_CREATE_REFLOG)`,
/// so no log line) and return its reflog to where it was.
#[allow(clippy::too_many_arguments)]
fn rename_rollback(
    repo: &gix::Repository,
    old: &str,
    orig_oid: gix::ObjectId,
    log: bool,
    logmoved: bool,
    sb_oldref: &std::path::Path,
    sb_newref: &std::path::Path,
    tmp_renamed_log: &std::path::Path,
) -> Result<bool> {
    let ref_path = files_base(repo, old).join(old);
    let lock = ref_path.with_file_name(format!(
        "{}.lock",
        ref_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    ));
    let restored = ref_path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| {
        std::fs::write(&lock, format!("{orig_oid}\n"))?;
        std::fs::rename(&lock, &ref_path)
    });
    if let Err(e) = restored {
        let _ = std::fs::remove_file(&lock);
        eprintln!("error: unable to lock {old} for rollback: {}", crate::external::strerror(&e));
    }
    if logmoved {
        if let Err(e) = std::fs::rename(sb_newref, sb_oldref) {
            eprintln!(
                "error: unable to restore logfile {old} from {}: {}",
                sb_newref.display(),
                crate::external::strerror(&e)
            );
        }
    }
    if !logmoved && log {
        if let Err(e) = std::fs::rename(tmp_renamed_log, sb_oldref) {
            eprintln!(
                "error: unable to restore logfile {old} from logs/{TMP_RENAMED_LOG}: {}",
                crate::external::strerror(&e)
            );
        }
    }
    Ok(false)
}

/// `refs_verify_refname_available(refname, NULL, skip = {skip})` (refs.c),
/// answering the one question a rename asks of it: which existing reference,
/// other than `skip`, stands in the way of creating `refname`? Either one at a
/// parent path, which would have to become a directory, or one below it, which
/// already made it one. git words both `'%s' exists; cannot create '%s'`.
fn refname_conflict(repo: &gix::Repository, refname: &str, skip: &str) -> Option<String> {
    let mut dir = refname;
    while let Some(cut) = dir.rfind('/') {
        dir = &dir[..cut];
        if dir != skip && crate::refname::ref_exists(repo, dir.as_bytes()) {
            return Some(dir.to_string());
        }
    }
    let prefix = format!("{refname}/");
    let platform = repo.references().ok()?;
    let below = platform.prefixed(prefix.as_str()).ok()?;
    below
        .filter_map(std::result::Result::ok)
        .map(|r| r.name().as_bstr().to_string())
        .find(|name| name != skip)
}

/// `remove_empty_directories()` (refs/files-backend.c): remove `path` if it is
/// a directory holding nothing but empty directories; anything else is left.
fn remove_empty_directories(path: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(path) else { return };
    for entry in entries.filter_map(std::result::Result::ok) {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            remove_empty_directories(&entry.path());
        }
    }
    let _ = std::fs::remove_dir(path);
}

/// Point the last reflog entry's *old* id at `old`: a rename records a reference
/// that changed name while pointing where it already pointed.
fn rewrite_last_reflog_old_id(path: &std::path::Path, old: gix::ObjectId) {
    let Ok(body) = std::fs::read(path) else { return };
    let Some(last_nl) = body.iter().rposition(|b| *b == b'\n') else { return };
    let start = body[..last_nl].iter().rposition(|b| *b == b'\n').map_or(0, |p| p + 1);
    let hex = old.to_hex().to_string();
    if body.len() < start + hex.len() {
        return;
    }
    let mut out = body;
    out[start..start + hex.len()].copy_from_slice(hex.as_bytes());
    let _ = std::fs::write(path, out);
}

#[cfg(test)]
mod exists_tests {
    use super::loose_contents_parse;
    use gix::hash::Kind;

    /// `parse_loose_ref_contents()` accepts a `ref:` line whatever follows it, and
    /// otherwise demands a full-width object id that ends its token.
    ///
    /// The `FETCH_HEAD` shape — an id followed by whitespace and more text — is the
    /// reason for the `*p != '\0' && !isspace(*p)` half of git's test rather than a
    /// plain length check.
    #[test]
    fn a_loose_ref_is_a_symref_line_or_an_object_id() {
        let sha1 = Kind::Sha1;
        let id = "5915d79de18d919476d339c8b8efda1d9bb166e2";
        for ok in [
            format!("{id}\n"),
            format!("{id}"),
            format!("{id}\t\n"),
            // `FETCH_HEAD` carries branch and remote after the id.
            format!("{id}\t\tbranch 'main' of /tmp/repo\n"),
            "ref: refs/heads/main\n".to_string(),
            // A dangling symref is still a ref that exists.
            "ref: refs/heads/gone\n".to_string(),
        ] {
            assert!(loose_contents_parse(ok.as_bytes(), sha1), "{ok:?} should parse");
        }
    }

    /// Everything else is `EINVAL`, which is what makes `git refs exists` and
    /// `git show-ref --exists` answer 1 rather than 2. Verified against stock git
    /// 2.55.0 with a loose ref file holding `garbage`: `error: failed to look up
    /// reference: Invalid argument`, exit 1.
    #[test]
    fn anything_else_is_the_einval_that_exits_one() {
        let sha1 = Kind::Sha1;
        for bad in [
            "garbage\n",
            "",
            "\n",
            // One hex digit short of the hash width.
            "5915d79de18d919476d339c8b8efda1d9bb166e\n",
            // Full width, but the token does not end there.
            "5915d79de18d919476d339c8b8efda1d9bb166e2x\n",
            // `ref:` is matched as a prefix of the line, not case-insensitively.
            "REF: refs/heads/main\n",
        ] {
            assert!(!loose_contents_parse(bad.as_bytes(), sha1), "{bad:?} should not parse");
        }
    }
}
