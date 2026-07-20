//! `git update-index` — the plumbing command that edits the index directly.
//!
//! Served natively through the vendored gitoxide crates so tools on PATH observe
//! the same staged index. Options are *positional state*, exactly as in stock
//! git: they are applied left-to-right and affect only the paths that follow, so
//! `--add a --no-add b` means what git means by it.
//!
//! Ported flags (stdout, stderr wording and exit codes match stock git):
//!   * `--add`, `--remove`, `--force-remove`, `--replace`, `--info-only`
//!     (and their `--no-` forms)
//!   * `--refresh`, `--really-refresh`, `-q`, `--unmerged`, `--ignore-missing`,
//!     `--ignore-submodules`, `--ignore-skip-worktree-entries`
//!   * `--cacheinfo <mode>,<object>,<path>` and the 3-argument legacy form
//!   * `--chmod=(+|-)x`
//!   * `--assume-unchanged` / `--no-assume-unchanged`
//!   * `--skip-worktree` / `--no-skip-worktree`
//!   * `--fsmonitor-valid` / `--no-fsmonitor-valid`
//!   * `--stdin` (must be the last argument, as in git) and `-z`
//!   * `-g`/`--again` and `--unresolve`, both of which swallow the remaining
//!     arguments as a pathspec exactly as git's callbacks do
//!   * `--clear-resolve-undo`, `--force-write-index`
//!   * `--index-version <n>` / `--show-index-version` (one shared variable in
//!     git, so a trailing `--show-index-version` cancels a bad `--index-version`)
//!   * `--split-index`, `--untracked-cache` and friends, `--fsmonitor`
//!   * `--verbose`, `--`, and `<file>...`
//!
//! Faithfully reproduced behaviours: git's `prefix_path` normalisation and its
//! "is outside repository" refusal, `verify_path` (the `Ignoring path <p>` note
//! for `.git` components and any trailing slash), the up-to-date short
//! circuit in `add_one_path`, the file/directory conflict diagnostics, gitlink
//! (submodule) registration for directory arguments, git's `PARSE_OPT_NOARG`
//! rejection of `--<flag>=<value>` on every non-value option, and the
//! `refresh_cache_ent` decision ladder — including that `--refresh` honours the
//! skip-worktree bit always and the assume-unchanged bit unless
//! `--really-refresh` is given, and that `-q` suppresses the
//! `<path>: needs update` lines and the exit-1 with them, while still reporting
//! `<path>: needs merge` for conflicted paths. `--test-untracked-cache` runs the
//! real filesystem probe and, like git, returns before the index is written.
//!
//! Deliberately NOT ported:
//!   * `--index-info` (a whole second input grammar), which bails with a precise
//!     reason rather than producing an index that merely looks right.
//!   * `core.ignoreStat=true`, which would silently flip the assume-unchanged
//!     bit on every entry git writes.
//!   * C-quoted paths on `--stdin` (git's `unquote_c_style` input form).
//!
//! Accepted but not represented on disk, because the vendored `gix_index` writes
//! neither the extension nor a pinned header version. Each is invisible to
//! `git status` / `git ls-files`, so behaviour observable through git itself is
//! unaffected, but the index bytes differ from stock git's:
//!   * `--index-version <n>`: the range check and the resulting index write are
//!     performed, but `gix_index` derives V2/V3 from the entry flags and cannot
//!     emit V4 or be pinned.
//!   * `--split-index`, `--untracked-cache`, `--fsmonitor`: the `link`, `UNTR`
//!     and `FSMN` extensions are not writable through the vendored crates.
//!   * `--unresolve` restores nothing when the index actually carries a `REUC`
//!     extension: `gix_index` decodes it but exposes no accessors on
//!     `resolve_undo::ResolvePath`, so the recorded stages cannot be read back.
//!     That case bails rather than silently doing nothing.
//!
//! Also note that content filters (`.gitattributes` clean/smudge, `autocrlf`)
//! are not applied when hashing worktree files, matching this port's `git add`.

use anyhow::{bail, Result};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stage, Stat};

/// git's `ce_match_stat_basic` change bits, kept separate because
/// `ie_modified` reacts differently to each.
const DATA_CHANGED: u8 = 1 << 0;
const MODE_CHANGED: u8 = 1 << 1;
const TYPE_CHANGED: u8 = 1 << 2;
const STAT_CHANGED: u8 = 1 << 3;

/// Which bit `--assume-unchanged` / `--skip-worktree` is toggling, and whether it
/// is being set or cleared.
#[derive(Clone, Copy)]
struct Mark {
    flag: Flags,
    set: bool,
}

/// Everything the left-to-right option scan mutates. One instance lives for the
/// whole invocation; `update_one` reads it for each path.
struct Ctx {
    repo: gix::Repository,
    index: gix::index::File,
    /// Repo-root-relative, lexically normalised worktree path (`None` when bare).
    workdir: Option<PathBuf>,
    /// Current subdirectory, `""` or `"sub/"`, prepended to relative arguments.
    prefix: String,
    /// The index has entry changes that must be persisted.
    dirty: bool,
    /// Entries were added/removed/re-flagged, so the cache-tree extension is stale.
    tree_stale: bool,
    /// `--refresh` reported at least one path; the command exits 1.
    has_errors: bool,
    /// `--force-write-index`: persist the index even when nothing changed.
    force_write: bool,

    allow_add: bool,
    allow_remove: bool,
    allow_replace: bool,
    force_remove: bool,
    info_only: bool,
    verbose: bool,
    ignore_skip_worktree_entries: bool,

    refresh_quiet: bool,
    allow_unmerged: bool,
    ignore_missing: bool,
    ignore_submodules: bool,

    mark_valid: Option<Mark>,
    mark_skip_worktree: Option<Mark>,
    mark_fsmonitor: Option<Mark>,
    set_executable_bit: Option<char>,

    /// `core.fileMode`; when false the executable bit of worktree files is ignored.
    trust_executable_bit: bool,
    stat_opts: gix::index::entry::stat::Options,
}

/// Signals that git would have called `die()`: the message is already on stderr
/// and the process must exit 128 without writing the index.
struct Die;

type Step = std::result::Result<(), Die>;

pub fn update_index(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    if repo.config_snapshot().boolean("core.ignoreStat") == Some(true) {
        bail!(
            "core.ignoreStat=true is not supported (it would set assume-unchanged on every entry)"
        );
    }

    let workdir = match repo.workdir() {
        Some(w) => Some(normalize_lexically(
            &std::fs::canonicalize(w).unwrap_or_else(|_| w.to_owned()),
        )),
        None => None,
    };
    let prefix = match repo.prefix()? {
        Some(p) if !p.as_os_str().is_empty() => {
            let s = p.to_string_lossy().replace('\\', "/");
            format!("{}/", s.trim_end_matches('/'))
        }
        _ => String::new(),
    };

    // Serialize the read-modify-write through the repo coordinator, as every
    // other index-mutating subcommand in this port does.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let index = if repo.index_path().exists() {
        repo.open_index()?
    } else {
        gix::index::File::from_state(
            gix::index::State::new(repo.object_hash()),
            repo.index_path(),
        )
    };
    let stat_opts = repo.stat_options()?;
    let trust_executable_bit = repo
        .config_snapshot()
        .boolean("core.fileMode")
        .unwrap_or(true);

    let mut ctx = Ctx {
        repo,
        index,
        workdir,
        prefix,
        dirty: false,
        tree_stale: false,
        has_errors: false,
        force_write: false,
        allow_add: false,
        allow_remove: false,
        allow_replace: false,
        force_remove: false,
        info_only: false,
        verbose: false,
        ignore_skip_worktree_entries: false,
        refresh_quiet: false,
        allow_unmerged: false,
        ignore_missing: false,
        ignore_submodules: false,
        mark_valid: None,
        mark_skip_worktree: None,
        mark_fsmonitor: None,
        set_executable_bit: None,
        trust_executable_bit,
        stat_opts,
    };

    match run(&mut ctx, args)? {
        Outcome::Die => Ok(ExitCode::from(128)),
        Outcome::Usage => Ok(ExitCode::from(129)),
        Outcome::Exit(code) => Ok(ExitCode::from(code)),
        Outcome::Done => {
            if ctx.dirty || ctx.force_write {
                if ctx.tree_stale {
                    ctx.index.remove_tree();
                }
                ctx.index.write(gix::index::write::Options::default())?;
            }
            Ok(if ctx.has_errors {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}

enum Outcome {
    /// Ran to completion; persist the index and report `has_errors`.
    Done,
    /// git `die()`d: exit 128, index untouched on disk.
    Die,
    /// git's option parser rejected the command line: exit 129.
    Usage,
    /// git returned before reaching the index write (`--test-untracked-cache`).
    Exit(u8),
}

/// git's `untracked_cache` enum: which of the mutually exclusive
/// untracked-cache options won the left-to-right scan.
#[derive(Clone, Copy, PartialEq)]
enum UntrackedCache {
    Unspecified,
    Disable,
    Enable,
    Force,
    Test,
}

/// The left-to-right option/path scan, mirroring `cmd_update_index`.
fn run(ctx: &mut Ctx, args: &[String]) -> Result<Outcome> {
    // git keeps `--index-version <n>` and `--show-index-version` in one
    // variable: 0 = neither, -1 = report, n > 0 = write that format. Whichever
    // comes last therefore wins, and a trailing `--show-index-version` really
    // does cancel an out-of-range `--index-version`.
    let mut preferred_index_format: i32 = 0;
    let mut untracked_cache = UntrackedCache::Unspecified;
    let mut nul_term_line = false;
    let mut end_of_opts = false;
    let mut i = 0;

    while i < args.len() {
        let a = args[i].as_str();

        if !end_of_opts && a == "--" {
            end_of_opts = true;
            i += 1;
            continue;
        }

        // Short options, including clusters like `-qz`.
        if !end_of_opts && a.len() > 1 && a.starts_with('-') && !a.starts_with("--") {
            let mut swallowed_rest = false;
            for c in a[1..].chars() {
                match c {
                    'z' => nul_term_line = true,
                    'q' => ctx.refresh_quiet = true,
                    'g' => {
                        if do_reupdate(ctx, &args[i + 1..])?.is_err() {
                            return Ok(Outcome::Die);
                        }
                        swallowed_rest = true;
                        break;
                    }
                    _ => {
                        eprintln!("error: unknown switch `{c}'");
                        return Ok(Outcome::Usage);
                    }
                }
            }
            i = if swallowed_rest { args.len() } else { i + 1 };
            continue;
        }

        if !end_of_opts && a.starts_with("--") && a.len() > 2 {
            let long = &a[2..];
            let (name, attached) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };

            // Every option but these two is `PARSE_OPT_NOARG`, and git rejects an
            // attached value on those outright rather than ignoring it.
            if attached.is_some() && !matches!(name, "chmod" | "index-version") {
                eprintln!("error: option `{name}' takes no value");
                return Ok(Outcome::Usage);
            }

            match name {
                // `--stdin` must be last; everything after it would be ignored,
                // so git refuses rather than silently dropping arguments.
                "stdin" => {
                    if i + 1 != args.len() {
                        eprintln!("error: option 'stdin' must be the last argument");
                        return Ok(Outcome::Usage);
                    }
                    if let Err(Die) = read_stdin_paths(ctx, nul_term_line)? {
                        return Ok(Outcome::Die);
                    }
                }

                "verbose" => ctx.verbose = true,
                "no-verbose" => ctx.verbose = false,
                "add" => ctx.allow_add = true,
                "no-add" => ctx.allow_add = false,
                "remove" => ctx.allow_remove = true,
                "no-remove" => ctx.allow_remove = false,
                "replace" => ctx.allow_replace = true,
                "no-replace" => ctx.allow_replace = false,
                "force-remove" => {
                    ctx.force_remove = true;
                    ctx.allow_remove = true;
                }
                "no-force-remove" => ctx.force_remove = false,
                "info-only" => ctx.info_only = true,
                "no-info-only" => ctx.info_only = false,
                "unmerged" => ctx.allow_unmerged = true,
                "no-unmerged" => ctx.allow_unmerged = false,
                "ignore-missing" => ctx.ignore_missing = true,
                "no-ignore-missing" => ctx.ignore_missing = false,
                "ignore-submodules" => ctx.ignore_submodules = true,
                "no-ignore-submodules" => ctx.ignore_submodules = false,
                "ignore-skip-worktree-entries" => ctx.ignore_skip_worktree_entries = true,
                "no-ignore-skip-worktree-entries" => ctx.ignore_skip_worktree_entries = false,
                "force-write-index" => ctx.force_write = true,
                "no-force-write-index" => ctx.force_write = false,

                "show-index-version" => preferred_index_format = -1,
                "no-show-index-version" | "no-index-version" => preferred_index_format = 0,
                "index-version" => {
                    let (value, consumed) = match attached {
                        Some(v) => (v, 1),
                        None => match args.get(i + 1) {
                            Some(v) => (v.as_str(), 2),
                            None => {
                                eprintln!("error: option `index-version' requires a value");
                                return Ok(Outcome::Usage);
                            }
                        },
                    };
                    match value.trim().parse::<i32>() {
                        Ok(n) => preferred_index_format = n,
                        Err(_) => {
                            eprintln!(
                                "error: option `index-version' expects a numerical value"
                            );
                            return Ok(Outcome::Usage);
                        }
                    }
                    i += consumed;
                    continue;
                }

                "assume-unchanged" => {
                    ctx.mark_valid = Some(Mark { flag: Flags::ASSUME_VALID, set: true })
                }
                "no-assume-unchanged" => {
                    ctx.mark_valid = Some(Mark { flag: Flags::ASSUME_VALID, set: false })
                }
                "skip-worktree" => {
                    ctx.mark_skip_worktree = Some(Mark { flag: Flags::SKIP_WORKTREE, set: true })
                }
                "no-skip-worktree" => {
                    ctx.mark_skip_worktree = Some(Mark { flag: Flags::SKIP_WORKTREE, set: false })
                }
                "fsmonitor-valid" => {
                    ctx.mark_fsmonitor = Some(Mark { flag: Flags::FSMONITOR_VALID, set: true })
                }
                "no-fsmonitor-valid" => {
                    ctx.mark_fsmonitor = Some(Mark { flag: Flags::FSMONITOR_VALID, set: false })
                }

                "refresh" => {
                    if refresh(ctx, false)?.is_err() {
                        return Ok(Outcome::Die);
                    }
                }
                "really-refresh" => {
                    if refresh(ctx, true)?.is_err() {
                        return Ok(Outcome::Die);
                    }
                }

                // Both callbacks consume every remaining argument as a pathspec,
                // so options after them are never parsed as options.
                "again" => {
                    if do_reupdate(ctx, &args[i + 1..])?.is_err() {
                        return Ok(Outcome::Die);
                    }
                    i = args.len();
                    continue;
                }
                "unresolve" => {
                    if do_unresolve(ctx, &args[i + 1..])?.is_err() {
                        return Ok(Outcome::Die);
                    }
                    i = args.len();
                    continue;
                }

                "clear-resolve-undo" => {
                    ctx.index.remove_resolve_undo();
                    ctx.dirty = true;
                }

                // The extensions below are not writable through the vendored
                // crates; see the module documentation. Accepting them keeps
                // exit codes and everything observable through git in step.
                "split-index" | "no-split-index" => {}
                "fsmonitor" => {
                    if ctx.repo.config_snapshot().string("core.fsmonitor").is_none() {
                        eprintln!(
                            "warning: core.fsmonitor is unset; set it if you really want to enable fsmonitor"
                        );
                    }
                }
                "no-fsmonitor" => {}
                "untracked-cache" => untracked_cache = UntrackedCache::Enable,
                "no-untracked-cache" => untracked_cache = UntrackedCache::Disable,
                "force-untracked-cache" => untracked_cache = UntrackedCache::Force,
                "test-untracked-cache" => untracked_cache = UntrackedCache::Test,
                "no-test-untracked-cache" | "no-force-untracked-cache" => {
                    untracked_cache = UntrackedCache::Unspecified
                }

                "cacheinfo" | "chmod" => {
                    let consumed = match option_with_value(ctx, name, attached, args, i)? {
                        Ok(n) => n,
                        Err(ParseFail::Die) => return Ok(Outcome::Die),
                        Err(ParseFail::Usage) => return Ok(Outcome::Usage),
                    };
                    i += consumed;
                    continue;
                }

                "index-info" => {
                    bail!("--index-info is not supported (its stdin grammar is not ported)")
                }

                _ => {
                    eprintln!("error: unknown option `{name}'");
                    return Ok(Outcome::Usage);
                }
            }
            i += 1;
            continue;
        }

        // A path argument.
        if let Err(Die) = handle_path(ctx, a)? {
            return Ok(Outcome::Die);
        }
        i += 1;
    }

    // git's tail, in its order: report or apply the index format, then the
    // untracked-cache probe, which returns before the index is ever written.
    if preferred_index_format != 0 {
        if preferred_index_format < 0 {
            println!("{}", version_number(ctx.index.version()));
        } else if !(2..=4).contains(&preferred_index_format) {
            eprintln!("fatal: index-version {preferred_index_format} not in range: 2..4");
            return Ok(Outcome::Die);
        } else {
            // git records the version and flags the index for rewrite; the
            // rewrite happens here too, but `gix_index` picks the header version
            // itself (see the module documentation).
            ctx.dirty = true;
        }
    }

    if untracked_cache == UntrackedCache::Test {
        return Ok(Outcome::Exit(u8::from(!test_untracked_cache_supported())));
    }

    Ok(Outcome::Done)
}

enum ParseFail {
    Die,
    Usage,
}

/// Handle `--cacheinfo` / `--chmod` in any of their spellings, returning how many
/// argv slots were consumed. `--cacheinfo` never has an `attached` value: it is
/// `PARSE_OPT_NOARG` and the caller has already rejected `--cacheinfo=<v>`.
fn option_with_value(
    ctx: &mut Ctx,
    name: &str,
    attached: Option<&str>,
    args: &[String],
    i: usize,
) -> Result<std::result::Result<usize, ParseFail>> {
    if name == "chmod" {
        let value = match attached {
            Some(v) => v,
            None => match args.get(i + 1) {
                Some(v) => v.as_str(),
                None => {
                    eprintln!("error: option 'chmod' requires a value");
                    return Ok(Err(ParseFail::Usage));
                }
            },
        };
        let flip = match value {
            "+x" => '+',
            "-x" => '-',
            _ => {
                eprintln!("error: option 'chmod' expects \"+x\" or \"-x\"");
                return Ok(Err(ParseFail::Usage));
            }
        };
        ctx.set_executable_bit = Some(flip);
        return Ok(Ok(if attached.is_some() { 1 } else { 2 }));
    }

    // --cacheinfo: prefer the single `<mode>,<object>,<path>` argument, falling
    // back to the legacy three-argument form exactly as git's callback does.
    let (single, consumed) = match attached {
        Some(v) => (Some(v.to_string()), 1usize),
        None => (args.get(i + 1).cloned(), 2usize),
    };
    if let Some(spec) = single.as_deref() {
        if let Some((mode, oid, path)) = parse_new_style_cacheinfo(spec) {
            return Ok(match add_cacheinfo(ctx, mode, oid, &path)? {
                Ok(()) => Ok(consumed),
                Err(Die) => Err(ParseFail::Die),
            });
        }
    }
    if attached.is_some() || args.len() < i + 4 {
        eprintln!("error: option 'cacheinfo' expects <mode>,<sha1>,<path>");
        return Ok(Err(ParseFail::Usage));
    }
    let mode = match u32::from_str_radix(args[i + 1].trim(), 8) {
        Ok(m) => m,
        Err(_) => {
            eprintln!(
                "fatal: git update-index: --cacheinfo cannot add {}",
                args[i + 3]
            );
            return Ok(Err(ParseFail::Die));
        }
    };
    let oid = match ObjectId::from_hex(args[i + 2].as_bytes()) {
        Ok(o) => o,
        Err(_) => {
            eprintln!(
                "fatal: git update-index: --cacheinfo cannot add {}",
                args[i + 3]
            );
            return Ok(Err(ParseFail::Die));
        }
    };
    Ok(match add_cacheinfo(ctx, mode, oid, &args[i + 3])? {
        Ok(()) => Ok(4),
        Err(Die) => Err(ParseFail::Die),
    })
}

/// `<mode>,<object>,<path>` — git's `parse_new_style_cacheinfo`.
fn parse_new_style_cacheinfo(spec: &str) -> Option<(u32, ObjectId, String)> {
    let (mode_s, rest) = spec.split_once(',')?;
    let (oid_s, path) = rest.split_once(',')?;
    let mode = u32::from_str_radix(mode_s.trim(), 8).ok()?;
    let oid = ObjectId::from_hex(oid_s.as_bytes()).ok()?;
    if path.is_empty() {
        return None;
    }
    Some((mode, oid, path.to_string()))
}

/// Register an entry with no filesystem backing (`--cacheinfo`).
///
/// Note that git does *not* run this path through `prefix_path`: the argument is
/// taken verbatim as a repository-root-relative path, so `--cacheinfo` from a
/// subdirectory registers the name exactly as written.
fn add_cacheinfo(ctx: &mut Ctx, raw_mode: u32, oid: ObjectId, raw_path: &str) -> Result<Step> {
    let path = BString::from(raw_path.as_bytes().to_vec());
    if !verify_path(path.as_bstr()) {
        eprintln!("error: Invalid path '{path}'");
        eprintln!("fatal: git update-index: --cacheinfo cannot add {path}");
        return Ok(Err(Die));
    }
    let mode = create_ce_mode(raw_mode);
    if !add_index_entry(ctx, path.as_bstr(), oid, mode, Stat::default())? {
        eprintln!("error: {path}: cannot add to the index - missing --add option?");
        eprintln!("fatal: git update-index: --cacheinfo cannot add {path}");
        return Ok(Err(Die));
    }
    report(ctx, format_args!("add '{path}'"));
    Ok(Ok(()))
}

/// Read `--stdin` paths (LF- or NUL-separated) and process each like an argv path.
fn read_stdin_paths(ctx: &mut Ctx, nul_term_line: bool) -> Result<Step> {
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;
    let sep = if nul_term_line { b'\0' } else { b'\n' };

    for line in buf.split(|&b| b == sep) {
        if line.is_empty() {
            continue;
        }
        let line = if !nul_term_line && line.last() == Some(&b'\r') {
            &line[..line.len() - 1]
        } else {
            line
        };
        let raw = line
            .to_str()
            .map_err(|_| anyhow::anyhow!("non-UTF-8 path on --stdin"))?;
        if !nul_term_line && raw.starts_with('"') {
            bail!("C-quoted paths on --stdin are not supported");
        }
        if let Err(Die) = handle_path(ctx, raw)? {
            return Ok(Err(Die));
        }
    }
    Ok(Ok(()))
}

/// One path argument: normalise it, run `update_one`, then apply a pending
/// `--chmod`, exactly in git's order.
fn handle_path(ctx: &mut Ctx, raw: &str) -> Result<Step> {
    let path = match resolve_path(ctx, raw)? {
        Ok(p) => p,
        Err(Die) => return Ok(Err(Die)),
    };
    if let Err(Die) = update_one(ctx, &path)? {
        return Ok(Err(Die));
    }
    if let Some(flip) = ctx.set_executable_bit {
        if let Err(Die) = chmod_path(ctx, flip, &path) {
            return Ok(Err(Die));
        }
    }
    Ok(Ok(()))
}

/// git's `update_one`: mark-only modes short-circuit, then `--force-remove`,
/// then the general add/remove path.
fn update_one(ctx: &mut Ctx, path: &BString) -> Result<Step> {
    let mark_only = ctx.mark_valid.is_some()
        || ctx.mark_skip_worktree.is_some()
        || ctx.mark_fsmonitor.is_some()
        || ctx.force_remove;

    // git lstats first, unless a mark-only mode makes the worktree irrelevant.
    let meta = if mark_only {
        None
    } else {
        match ctx.workdir.as_ref() {
            None => bail!("this operation must be run in a work tree"),
            Some(_) => match ctx.repo.workdir_path(path.as_bstr()) {
                Some(abs) => match gix::index::fs::Metadata::from_path_no_follow(&abs) {
                    Ok(m) => Some(Ok(m)),
                    Err(e) => Some(Err(e)),
                },
                None => bail!("this operation must be run in a work tree"),
            },
        }
    };
    if !verify_path(path.as_bstr()) {
        eprintln!("Ignoring path {path}");
        return Ok(Ok(()));
    }

    if let Some(mark) = ctx.mark_valid {
        return Ok(mark_ce_flags(ctx, path, mark));
    }
    if let Some(mark) = ctx.mark_skip_worktree {
        return Ok(mark_ce_flags(ctx, path, mark));
    }
    if let Some(mark) = ctx.mark_fsmonitor {
        return Ok(mark_ce_flags(ctx, path, mark));
    }

    if ctx.force_remove {
        remove_path_entries(ctx, path.as_bstr());
        report(ctx, format_args!("remove '{path}'"));
        return Ok(Ok(()));
    }

    match process_path(ctx, path, meta)? {
        Ok(()) => {
            report(ctx, format_args!("add '{path}'"));
            Ok(Ok(()))
        }
        Err(Die) => {
            eprintln!("fatal: Unable to process path {path}");
            Ok(Err(Die))
        }
    }
}

/// git's `process_path`. Errors here are the `error: ...` lines; the caller adds
/// the `fatal: Unable to process path ...` line.
fn process_path(
    ctx: &mut Ctx,
    path: &BString,
    meta: Option<std::result::Result<gix::index::fs::Metadata, std::io::Error>>,
) -> Result<Step> {
    let existing = ctx
        .index
        .entry_index_by_path_and_stage(path.as_bstr(), Stage::Unconflicted);

    // A skip-worktree entry promises the worktree is irrelevant, so it is never
    // re-read; only removal is meaningful.
    if let Some(idx) = existing {
        if ctx.index.entries()[idx]
            .flags
            .contains(Flags::SKIP_WORKTREE)
        {
            if !ctx.ignore_skip_worktree_entries && ctx.allow_remove {
                remove_path_entries(ctx, path.as_bstr());
            }
            return Ok(Ok(()));
        }
    }

    let meta = match meta {
        Some(Ok(m)) => m,
        Some(Err(e)) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                return Ok(remove_one_path(ctx, path));
            }
            eprintln!("error: lstat(\"{path}\"): {e}");
            return Ok(Err(Die));
        }
        None => unreachable!("update_one always stats before reaching process_path"),
    };

    if meta.is_dir() {
        return process_directory(ctx, path, &meta);
    }
    add_one_path(ctx, existing, path, &meta)
}

/// git's `remove_one_path`.
fn remove_one_path(ctx: &mut Ctx, path: &BString) -> Step {
    if !ctx.allow_remove {
        eprintln!("error: {path}: does not exist and --remove not passed");
        return Err(Die);
    }
    remove_path_entries(ctx, path.as_bstr());
    Ok(())
}

/// git's `process_directory`: a directory argument is only meaningful as a
/// submodule gitlink, otherwise it is an error.
fn process_directory(
    ctx: &mut Ctx,
    path: &BString,
    meta: &gix::index::fs::Metadata,
) -> Result<Step> {
    let abs = match ctx.repo.workdir_path(path.as_bstr()) {
        Some(a) => a,
        None => bail!("this operation must be run in a work tree"),
    };
    let existing = ctx
        .index
        .entry_index_by_path_and_stage(path.as_bstr(), Stage::Unconflicted);

    if let Some(idx) = existing {
        if ctx.index.entries()[idx].mode == Mode::COMMIT {
            // No HEAD in the nested repository means there is nothing to record.
            let Some(head) = gitlink_head(&abs) else {
                return Ok(Ok(()));
            };
            let stat = Stat::from_fs(meta)?;
            if ctx.index.entries()[idx].id == head && ctx.index.entries()[idx].stat == stat {
                return Ok(Ok(()));
            }
            {
                let e = &mut ctx.index.entries_mut()[idx];
                e.id = head;
                e.stat = stat;
                e.mode = Mode::COMMIT;
            }
            ctx.dirty = true;
            ctx.tree_stale = true;
            return Ok(Ok(()));
        }
        return Ok(remove_one_path(ctx, path));
    }

    if let Some(head) = gitlink_head(&abs) {
        let stat = Stat::from_fs(meta)?;
        if !add_index_entry(ctx, path.as_bstr(), head, Mode::COMMIT, stat)? {
            eprintln!("error: {path}: cannot add to the index - missing --add option?");
            return Ok(Err(Die));
        }
        return Ok(Ok(()));
    }

    eprintln!("error: {path}: is a directory - add individual files instead");
    Ok(Err(Die))
}

/// The `HEAD` commit of a nested repository, or `None` if there is no repository
/// there or it has no commit yet.
fn gitlink_head(abs: &Path) -> Option<ObjectId> {
    let sub = gix::open(abs).ok()?;
    sub.head_id().ok().map(|id| id.detach())
}

/// git's `add_one_path`: hash the worktree file and (re)write its stage-0 entry,
/// short-circuiting when the existing entry is already up to date.
fn add_one_path(
    ctx: &mut Ctx,
    existing: Option<usize>,
    path: &BString,
    meta: &gix::index::fs::Metadata,
) -> Result<Step> {
    let new_stat = Stat::from_fs(meta)?;

    if let Some(idx) = existing {
        let (flags, old_stat, old_mode) = {
            let e = &ctx.index.entries()[idx];
            (e.flags, e.stat, e.mode)
        };
        // `ie_match_stat` reports "unchanged" for an assume-unchanged entry
        // whatever the worktree says, so the entry (and its bit) survive intact.
        if flags.contains(Flags::ASSUME_VALID) {
            return Ok(Ok(()));
        }
        if old_stat.matches(&new_stat, ctx.stat_opts)
            && old_mode == ce_mode_from_stat(ctx, Some(old_mode), meta)
        {
            return Ok(Ok(())); // already up to date
        }
    }

    let old_mode = existing.map(|idx| ctx.index.entries()[idx].mode);
    let mode = ce_mode_from_stat(ctx, old_mode, meta);

    let abs = match ctx.repo.workdir_path(path.as_bstr()) {
        Some(a) => a,
        None => bail!("this operation must be run in a work tree"),
    };
    let content = read_worktree_content(&abs, meta)?;

    // `--info-only` records the object id without ever creating the object.
    let id = if ctx.info_only {
        gix::objs::compute_hash(ctx.repo.object_hash(), gix::objs::Kind::Blob, &content)?
    } else {
        ctx.repo.write_blob(&content)?.detach()
    };

    if !add_index_entry(ctx, path.as_bstr(), id, mode, new_stat)? {
        eprintln!("error: {path}: cannot add to the index - missing --add option?");
        return Ok(Err(Die));
    }
    Ok(Ok(()))
}

/// The bytes git would hash for this worktree item: link target for symlinks,
/// file contents otherwise (no `.gitattributes` filtering — see the module doc).
fn read_worktree_content(abs: &Path, meta: &gix::index::fs::Metadata) -> Result<Vec<u8>> {
    if meta.is_symlink() {
        let target = std::fs::read_link(abs)?;
        #[cfg(unix)]
        let bytes = {
            use std::os::unix::ffi::OsStrExt;
            target.as_os_str().as_bytes().to_vec()
        };
        #[cfg(not(unix))]
        let bytes = target.to_string_lossy().into_owned().into_bytes();
        Ok(bytes)
    } else {
        Ok(std::fs::read(abs)?)
    }
}

/// git's `ce_mode_from_stat`, honouring `core.fileMode`.
fn ce_mode_from_stat(ctx: &Ctx, old: Option<Mode>, meta: &gix::index::fs::Metadata) -> Mode {
    if meta.is_symlink() {
        return Mode::SYMLINK;
    }
    if !ctx.trust_executable_bit {
        return match old {
            Some(m) if m == Mode::FILE || m == Mode::FILE_EXECUTABLE => m,
            _ => Mode::FILE,
        };
    }
    if meta.is_executable() {
        Mode::FILE_EXECUTABLE
    } else {
        Mode::FILE
    }
}

/// git's `add_index_entry_with_check`, reduced to the flags update-index passes.
/// Returns `false` when git would have returned -1 (the caller prints the
/// `cannot add to the index` line).
fn add_index_entry(
    ctx: &mut Ctx,
    path: &BStr,
    id: ObjectId,
    mode: Mode,
    stat: Stat,
) -> Result<bool> {
    // Exact stage-0 match: replace in place, no flags required.
    if let Some(idx) = ctx
        .index
        .entry_index_by_path_and_stage(path, Stage::Unconflicted)
    {
        let unchanged = {
            let e = &mut ctx.index.entries_mut()[idx];
            let same = e.id == id && e.mode == mode && e.stat == stat && e.flags.is_empty();
            e.id = id;
            e.mode = mode;
            e.stat = stat;
            e.flags = Flags::empty();
            same
        };
        if !unchanged {
            ctx.dirty = true;
            ctx.tree_stale = true;
        }
        return Ok(true);
    }

    // A stage-0 insertion always displaces the conflicted stages of that path,
    // and doing so makes the add legal even without `--add`.
    let owned = path.to_owned();
    let mut removed_conflicted = false;
    ctx.index.remove_entries(|_, p, _| {
        let hit = p == owned.as_bstr();
        removed_conflicted |= hit;
        hit
    });
    if removed_conflicted {
        ctx.dirty = true;
        ctx.tree_stale = true;
    }

    if !ctx.allow_add && !removed_conflicted {
        return Ok(false);
    }

    // File/directory conflict: either `path` shadows existing entries below it,
    // or an ancestor of `path` is itself a tracked file.
    let conflicting: Vec<BString> = {
        let backing = ctx.index.path_backing();
        let mut dir_prefix = owned.to_vec();
        dir_prefix.push(b'/');
        ctx.index
            .entries()
            .iter()
            .map(|e| e.path_in(backing))
            .filter(|p| p.starts_with(&dir_prefix) || is_ancestor_entry(p, owned.as_bstr()))
            .map(|p| p.to_owned())
            .collect()
    };
    if !conflicting.is_empty() {
        if !ctx.allow_replace {
            eprintln!("error: '{path}' appears as both a file and as a directory");
            return Ok(false);
        }
        ctx.index
            .remove_entries(|_, p, _| conflicting.iter().any(|c| c.as_bstr() == p));
        ctx.dirty = true;
        ctx.tree_stale = true;
    }

    ctx.index
        .dangerously_push_entry(stat, id, Flags::empty(), mode, path);
    ctx.index.sort_entries();
    ctx.dirty = true;
    ctx.tree_stale = true;
    Ok(true)
}

/// Whether the tracked file `candidate` is a strict directory prefix of `path`
/// (i.e. `path` would have to live *inside* a file).
fn is_ancestor_entry(candidate: &BStr, path: &BStr) -> bool {
    candidate.len() < path.len() && path.starts_with(candidate) && path[candidate.len()] == b'/'
}

/// git's `mark_ce_flags` — set or clear one flag on the stage-0 entry.
fn mark_ce_flags(ctx: &mut Ctx, path: &BString, mark: Mark) -> Step {
    let Some(idx) = ctx
        .index
        .entry_index_by_path_and_stage(path.as_bstr(), Stage::Unconflicted)
    else {
        eprintln!("fatal: Unable to mark file {path}");
        return Err(Die);
    };
    {
        let e = &mut ctx.index.entries_mut()[idx];
        e.flags.set(mark.flag, mark.set);
        normalize_extended(&mut e.flags);
    }
    ctx.dirty = true;
    ctx.tree_stale = true;
    Ok(())
}

/// git's `chmod_path` — flip the executable bit of a tracked regular file.
fn chmod_path(ctx: &mut Ctx, flip: char, path: &BString) -> Step {
    let idx = ctx
        .index
        .entry_index_by_path_and_stage(path.as_bstr(), Stage::Unconflicted);
    let Some(idx) = idx else {
        eprintln!("fatal: git update-index: cannot chmod {flip}x '{path}'");
        return Err(Die);
    };
    let current = ctx.index.entries()[idx].mode;
    if current != Mode::FILE && current != Mode::FILE_EXECUTABLE {
        eprintln!("fatal: git update-index: cannot chmod {flip}x '{path}'");
        return Err(Die);
    }
    let want = if flip == '+' {
        Mode::FILE_EXECUTABLE
    } else {
        Mode::FILE
    };
    if current != want {
        ctx.index.entries_mut()[idx].mode = want;
        ctx.dirty = true;
        ctx.tree_stale = true;
    }
    report(ctx, format_args!("chmod {flip}x '{path}'"));
    Ok(())
}

/// Drop every stage of `path` from the index.
fn remove_path_entries(ctx: &mut Ctx, path: &BStr) {
    let owned = path.to_owned();
    let mut removed = false;
    ctx.index.remove_entries(|_, p, _| {
        let hit = p == owned.as_bstr();
        removed |= hit;
        hit
    });
    if removed {
        ctx.dirty = true;
        ctx.tree_stale = true;
    }
}

/// git's `refresh_index`: re-`lstat` every entry, silently repair stale stat data
/// whose content still matches, and report the rest as `<path>: needs update`.
fn refresh(ctx: &mut Ctx, really: bool) -> Result<Step> {
    if ctx.workdir.is_none() {
        bail!("this operation must be run in a work tree");
    }

    let mut i = 0;
    while i < ctx.index.entries().len() {
        let (path, id, mode, flags, stat, stage) = {
            let backing = ctx.index.path_backing();
            let e = &ctx.index.entries()[i];
            (
                e.path_in(backing).to_owned(),
                e.id,
                e.mode,
                e.flags,
                e.stat,
                e.stage_raw(),
            )
        };

        if ctx.ignore_submodules && mode == Mode::COMMIT {
            i += 1;
            continue;
        }

        // Conflicted paths cannot be refreshed; skip all their stages at once.
        if stage != 0 {
            while i < ctx.index.entries().len() {
                let backing = ctx.index.path_backing();
                if ctx.index.entries()[i].path_in(backing) != path.as_bstr() {
                    break;
                }
                i += 1;
            }
            // Note that `-q` deliberately does *not* silence this one; git
            // reports unmerged paths regardless.
            if !ctx.allow_unmerged {
                println!("{path}: needs merge");
                ctx.has_errors = true;
            }
            continue;
        }

        // The skip-worktree bit is always honoured; assume-unchanged only until
        // `--really-refresh` tells us to stat regardless.
        if flags.contains(Flags::SKIP_WORKTREE) || (!really && flags.contains(Flags::ASSUME_VALID))
        {
            i += 1;
            continue;
        }

        let abs = match ctx.repo.workdir_path(path.as_bstr()) {
            Some(a) => a,
            None => bail!("this operation must be run in a work tree"),
        };
        let meta = match gix::index::fs::Metadata::from_path_no_follow(&abs) {
            Ok(m) => m,
            Err(e) => {
                if ctx.ignore_missing && e.kind() == std::io::ErrorKind::NotFound {
                    i += 1;
                    continue;
                }
                if !ctx.refresh_quiet {
                    println!("{path}: needs update");
                    ctx.has_errors = true;
                }
                i += 1;
                continue;
            }
        };

        // Gitlinks ignore stat data entirely: only the nested HEAD matters.
        if mode == Mode::COMMIT {
            let ok = meta.is_dir() && gitlink_head(&abs) == Some(id);
            if !ok && !ctx.refresh_quiet {
                println!("{path}: needs update");
                ctx.has_errors = true;
            }
            i += 1;
            continue;
        }

        let new_stat = Stat::from_fs(&meta)?;
        let changed = match_stat_basic(ctx, mode, id, stat, &new_stat, &meta);
        if changed == 0 {
            i += 1;
            continue;
        }

        // git refuses to refresh a mode/type change outright, and trusts a size
        // difference without re-reading the file (unless the size was never
        // recorded, as after `--cacheinfo`).
        let must_report = (changed & (MODE_CHANGED | TYPE_CHANGED)) != 0
            || ((changed & DATA_CHANGED) != 0 && stat.size != 0);

        let up_to_date = if must_report {
            false
        } else {
            match worktree_blob_id(ctx, &abs, mode, &meta)? {
                Some(disk) => disk == id,
                None => false,
            }
        };

        if up_to_date {
            ctx.index.entries_mut()[i].stat = new_stat;
            ctx.dirty = true;
        } else if !ctx.refresh_quiet {
            println!("{path}: needs update");
            ctx.has_errors = true;
        }
        i += 1;
    }
    Ok(Ok(()))
}

/// git's `do_reupdate` (`-g` / `--again`): re-run `update_one` on every stage-0
/// entry whose recorded mode and object differ from `HEAD`, limited to `specs`.
///
/// git's callback swallows every remaining argument as the pathspec, which is
/// why `git update-index -g --no-split-index` treats `--no-split-index` as a
/// path that matches nothing rather than as an option.
fn do_reupdate(ctx: &mut Ctx, specs: &[String]) -> Result<Step> {
    if ctx.workdir.is_none() {
        bail!("this operation must be run in a work tree");
    }
    // `PATHSPEC_PREFER_CWD`: a bare spec is relative to the current directory.
    let specs: Vec<String> = specs.iter().map(|s| format!("{}{s}", ctx.prefix)).collect();

    let candidates: Vec<(BString, Mode, ObjectId)> = {
        let backing = ctx.index.path_backing();
        ctx.index
            .entries()
            .iter()
            .filter(|e| e.stage_raw() == 0)
            .map(|e| (e.path_in(backing).to_owned(), e.mode, e.id))
            .filter(|(p, _, _)| pathspec_matches(&specs, p.as_bstr()))
            .collect()
    };

    // Scoped so the `HEAD` tree's borrow of the repository ends before the
    // mutable pass below.
    let stale: Vec<BString> = {
        let head_tree = ctx.repo.head_tree().ok();
        candidates
            .into_iter()
            .filter(|(path, mode, id)| {
                let Some(tree) = head_tree.as_ref() else {
                    return true; // unborn HEAD: git's `has_head == 0`
                };
                let path_str = path.to_str_lossy().into_owned();
                match tree.lookup_entry_by_path(Path::new(&path_str)) {
                    Ok(Some(e)) => {
                        u32::from(e.mode().value()) != mode.bits() || e.object_id() != *id
                    }
                    _ => true,
                }
            })
            .map(|(path, _, _)| path)
            .collect()
    };

    for path in stale {
        if let Err(Die) = update_one(ctx, &path)? {
            return Ok(Err(Die));
        }
    }
    Ok(Ok(()))
}

/// Literal pathspec match: an empty spec list matches everything, otherwise an
/// entry matches a spec it equals or lies underneath. git's glob and `:(magic)`
/// forms are not honoured here.
fn pathspec_matches(specs: &[String], path: &BStr) -> bool {
    if specs.is_empty() {
        return true;
    }
    specs.iter().any(|spec| {
        let s = spec.trim_end_matches('/').as_bytes();
        s.is_empty()
            || (path.len() == s.len() && path.starts_with(s))
            || (path.len() > s.len() && path.starts_with(s) && path[s.len()] == b'/')
    })
}

/// git's `do_unresolve` (`--unresolve`), which also swallows the remaining
/// arguments as its path list.
///
/// The vendored `gix_index` decodes the `REUC` extension but its
/// `resolve_undo::ResolvePath` records have private fields and no accessors, so
/// the saved stages cannot be read back. With no `REUC` present there is
/// nothing to restore and git is a no-op too; otherwise this refuses rather
/// than reporting a success it did not perform.
fn do_unresolve(ctx: &Ctx, _specs: &[String]) -> Result<Step> {
    match ctx.index.resolve_undo() {
        Some(recorded) if !recorded.is_empty() => bail!(
            "--unresolve cannot restore stages: gix_index decodes the resolve-undo (REUC) \
             extension but exposes no accessors on its records"
        ),
        _ => Ok(Ok(())),
    }
}

/// git's `test_if_untracked_cache_is_supported`: make a scratch directory in the
/// current directory and check that its stat data reacts the way the untracked
/// cache depends on. Progress goes to stderr, and the caller returns before the
/// index is written — `--test-untracked-cache --force-remove <p>` really does
/// leave the index alone.
fn test_untracked_cache_supported() -> bool {
    let cwd = std::env::current_dir().unwrap_or_default();
    let dir = cwd.join(format!("mtime-test-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir(&dir) {
        eprintln!("fatal: Could not make temporary directory: {e}");
        return false;
    }
    eprint!("Testing mtime in '{}' ", cwd.display());
    let supported = match probe_directory_mtime(&dir) {
        Ok(()) => {
            eprintln!(" OK");
            true
        }
        Err(why) => {
            eprintln!("\n{why}");
            false
        }
    };
    // git deletes the scratch directory from an atexit handler; leaving it
    // behind would surface as an untracked path.
    let _ = std::fs::remove_dir_all(&dir);
    supported
}

/// The six checks git makes, with its `avoid_racy()` one-second waits — those
/// exist for filesystem timestamp granularity, not politeness.
fn probe_directory_mtime(dir: &Path) -> std::result::Result<(), String> {
    let stat = |d: &Path| -> std::result::Result<std::time::SystemTime, String> {
        std::fs::metadata(d)
            .and_then(|m| m.modified())
            .map_err(|e| format!("failed to stat {}: {e}", d.display()))
    };
    let wait = || std::thread::sleep(std::time::Duration::from_secs(1));
    let io = |e: std::io::Error| e.to_string();

    let file = dir.join("newfile");
    let sub = dir.join("new-dir");
    let mut base = stat(dir)?;
    eprint!(".");

    wait();
    std::fs::write(&file, "").map_err(io)?;
    let now = stat(dir)?;
    if now == base {
        return Err("directory stat info does not change after adding a new file".into());
    }
    base = now;
    eprint!(".");

    wait();
    std::fs::create_dir(&sub).map_err(io)?;
    let now = stat(dir)?;
    if now == base {
        return Err("directory stat info does not change after adding a new directory".into());
    }
    base = now;
    eprint!(".");

    wait();
    std::fs::write(&file, "data").map_err(io)?;
    if stat(dir)? != base {
        return Err("directory stat info changes after adding a new file".into());
    }
    eprint!(".");

    wait();
    std::fs::remove_file(&file).map_err(io)?;
    let now = stat(dir)?;
    if now == base {
        return Err("directory stat info does not change after deleting a file".into());
    }
    base = now;
    eprint!(".");

    wait();
    std::fs::remove_dir(&sub).map_err(io)?;
    if stat(dir)? == base {
        return Err("directory stat info does not change after deleting a directory".into());
    }
    eprint!(".");
    Ok(())
}

/// git's `ce_match_stat_basic`, expressed over gitoxide's stat comparison.
fn match_stat_basic(
    ctx: &Ctx,
    mode: Mode,
    id: ObjectId,
    stat: Stat,
    new_stat: &Stat,
    meta: &gix::index::fs::Metadata,
) -> u8 {
    let mut changed = 0u8;
    if mode == Mode::SYMLINK {
        if !meta.is_symlink() {
            changed |= TYPE_CHANGED;
        }
    } else if !meta.is_file() {
        changed |= TYPE_CHANGED;
    } else if ctx.trust_executable_bit && ((mode == Mode::FILE_EXECUTABLE) != meta.is_executable())
    {
        changed |= MODE_CHANGED;
    }

    if stat.size != new_stat.size {
        changed |= DATA_CHANGED;
    }
    if !stat.matches(new_stat, ctx.stat_opts) {
        changed |= STAT_CHANGED;
    }
    // A zero recorded size on a non-empty blob is the racy-smudge marker: the
    // stat data was never filled in, so the content must be read.
    if stat.size == 0 && id != ctx.repo.object_hash().empty_blob() {
        changed |= DATA_CHANGED;
    }
    changed
}

/// Hash the worktree item at `abs` the way git would store it, for the content
/// comparison `--refresh` falls back to.
fn worktree_blob_id(
    ctx: &Ctx,
    abs: &Path,
    mode: Mode,
    meta: &gix::index::fs::Metadata,
) -> Result<Option<ObjectId>> {
    if mode == Mode::COMMIT {
        return Ok(gitlink_head(abs));
    }
    let content = read_worktree_content(abs, meta)?;
    Ok(Some(gix::objs::compute_hash(
        ctx.repo.object_hash(),
        gix::objs::Kind::Blob,
        &content,
    )?))
}

/// Print a `report()` line — only under `--verbose`, like git.
fn report(ctx: &Ctx, args: std::fmt::Arguments<'_>) {
    if ctx.verbose {
        println!("{args}");
    }
}

/// git's `prefix_path`: join a command-line path onto the current subdirectory,
/// normalise `.`, `..` and `//` lexically, and refuse anything that escapes the
/// worktree. A trailing slash is preserved so `verify_path` can reject it.
fn resolve_path(ctx: &Ctx, raw: &str) -> Result<std::result::Result<BString, Die>> {
    let Some(workdir) = ctx.workdir.as_ref() else {
        bail!("this operation must be run in a work tree");
    };

    let joined: PathBuf = if raw.starts_with('/') {
        PathBuf::from(raw)
    } else {
        workdir.join(&ctx.prefix).join(raw)
    };
    let abs = normalize_lexically(&joined);

    let Ok(rel) = abs.strip_prefix(workdir) else {
        eprintln!(
            "fatal: '{raw}' is outside repository at '{}'",
            workdir.display()
        );
        return Ok(Err(Die));
    };

    let mut out = rel.to_string_lossy().replace('\\', "/");
    if raw.ends_with('/') && !out.is_empty() {
        out.push('/');
    }
    Ok(Ok(BString::from(out.into_bytes())))
}

/// Resolve `.` and `..` textually, without touching the filesystem.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// git's `verify_path`: no `.`/`..`/`.git` component and no empty component,
/// which includes a trailing slash — `git update-index src/` reports
/// `Ignoring path src/` even though `src` is a real directory, and
/// `--cacheinfo 040000,<tree>,foo/` is rejected the same way.
fn verify_path(path: &BStr) -> bool {
    if path.is_empty() || path[0] == b'/' {
        return false;
    }
    for c in path.split(|&b| b == b'/') {
        if c.is_empty() {
            return false;
        }
        if c == b".".as_slice() || c == b"..".as_slice() || c.eq_ignore_ascii_case(b".git") {
            return false;
        }
    }
    true
}

/// git's `create_ce_mode` for a raw `--cacheinfo` mode.
fn create_ce_mode(mode: u32) -> Mode {
    if (mode & 0o170000) == 0o120000 {
        Mode::SYMLINK
    } else if is_dir_mode(mode) {
        Mode::COMMIT
    } else if (mode & 0o100) != 0 {
        Mode::FILE_EXECUTABLE
    } else {
        Mode::FILE
    }
}

/// Whether a raw mode names a directory or a gitlink.
fn is_dir_mode(mode: u32) -> bool {
    let kind = mode & 0o170000;
    kind == 0o040000 || kind == 0o160000
}

/// Keep the `EXTENDED` bit in sync with the extended flags that require it, the
/// way git recomputes `CE_EXTENDED` when writing an entry.
fn normalize_extended(flags: &mut Flags) {
    let needs = flags.intersects(Flags::SKIP_WORKTREE | Flags::INTENT_TO_ADD);
    flags.set(Flags::EXTENDED, needs);
}

fn version_number(v: gix::index::Version) -> u8 {
    match v {
        gix::index::Version::V2 => 2,
        gix::index::Version::V3 => 3,
        gix::index::Version::V4 => 4,
    }
}
