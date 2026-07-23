//! `git checkout-index` — copy files from the index to the working tree.
//!
//! This is a port of `builtin/checkout-index.c` plus the parts of `entry.c`
//! (`checkout_entry_ca` / `write_entry`) that it drives. Every documented flag is
//! implemented: `-a/--all`, `-f/--force`, `-q/--quiet`, `-n/--no-create` and its
//! `--create` opposite, `-u/--index`, `-z`, `--stdin`, `--temp`,
//! `--prefix=<string>`, `--stage=<1|2|3|all>`, `--ignore-skip-worktree-bits`, and
//! the `--` terminator. Short flags may be grouped (`-fa`); the boolean long
//! flags accept their generated `--no-` forms.
//!
//! Reproduced byte-for-byte from stock git: the `--temp` listing on stdout (both
//! the one-name and the `--stage=all` three-name form, `-z` record separators,
//! path names made relative to the current directory and C-quoted), the stderr
//! diagnostics (`git checkout-index: <path> is not in the cache` / `is unmerged`
//! / `does not exist at stage <n>` / `has skip-worktree enabled`, and
//! `<path> already exists, no checkout`), the `die()` messages, and the exit
//! codes: 0 on success, 1 when any path failed, 128 for a fatal argument error,
//! 129 for an unknown option (which also prints git's usage block).
//!
//! The "does this file need rewriting" test is git's `ie_match_stat`: entry mode
//! versus the file type and executable bit, then the configured stat comparison
//! (`core.trustctime`, `core.checkStat`), the racily-smudged zero-size rule, and
//! finally the racy-timestamp fallback which re-reads the file and compares its
//! blob id. Blob content passes through the repository's smudge pipeline
//! (`convert_to_worktree`), so `eol`, `working-tree-encoding`, `ident` and
//! external filter drivers apply exactly as they do for stock git.
//!
//! Not covered, each rejected rather than silently diverging:
//!   * sparse-directory index entries (cone-mode sparse checkout) — git expands
//!     them in place, which this port does not implement.
//!
//! A gitlink (submodule) entry only gets its directory created, which is what
//! git does here too: `checkout-index` never sets `recurse_submodules`, so git's
//! `submodule_from_ce()` always returns `NULL` and it also only calls `mkdir`.
//! Submodule content is populated by neither.
//!
//! Two known divergences from stock git, both narrow:
//!   * An existing submodule directory counts as unchanged here. git additionally
//!     compares the submodule's `HEAD` against the entry id, so for a populated
//!     submodule that has moved, git reports `already exists, no checkout` where
//!     this port stays silent.
//!   * The racy-timestamp content re-check reads the file at the `--prefix`ed
//!     destination; git reads the un-prefixed worktree path. This can only
//!     differ when `--prefix` is combined with a racy index entry.

use anyhow::{bail, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::stat::Options as StatOptions;
use gix::index::entry::{Mode, Stat};

/// git's `CHECKOUT_ALL` sentinel for `--stage=all`.
const CHECKOUT_ALL: u32 = 4;

/// git's usage block, printed verbatim alongside an unknown-option error.
const USAGE: &str = "usage: git checkout-index [<options>] [--] [<file>...]

    -a, --[no-]all        check out all files in the index
    --[no-]ignore-skip-worktree-bits
                          do not skip files with skip-worktree set
    -f, --[no-]force      force overwrite of existing files
    -q, --[no-]quiet      no warning for existing files and files not in index
    -n, --no-create       don't checkout new files
    --create              opposite of --no-create
    -u, --[no-]index      update stat information in the index file
    -z                    paths are separated with NUL character
    --[no-]stdin          read list of paths from the standard input
    --[no-]temp           write the content to temporary files
    --[no-]prefix <string>
                          when creating files, prepend <string>
    --stage (1|2|3|all)   copy out the files from named stage

";

/// Parsed command line for one invocation.
#[derive(Default)]
struct Opts {
    all: bool,                  // -a/--all
    force: bool,                // -f/--force
    quiet: bool,                // -q/--quiet
    not_new: bool,              // -n/--no-create
    refresh_cache: bool,        // -u/--index
    ignore_skip_worktree: bool, // --ignore-skip-worktree-bits
    to_tempfile: bool,          // --temp (implied by --stage=all)
    nul_term: bool,             // -z
    from_stdin: bool,           // --stdin
    base_dir: String,           // --prefix=<string>
    stage: u32,                 // 0..=3, or CHECKOUT_ALL
}

/// One index entry, lifted out of the index so the index stays free to be
/// mutated for `-u` after all files have been written.
struct Ent {
    path: BString,
    mode: Mode,
    id: ObjectId,
    stat: Stat,
    stage: u32,
    skip_worktree: bool,
    /// git's `is_racy_timestamp()`: the entry's mtime is at or after the index
    /// timestamp, so a matching stat is not proof that the content matches.
    racy: bool,
}

/// Everything the per-entry checkout needs, gathered once.
struct Ctx<'repo> {
    repo: &'repo gix::Repository,
    pipeline: gix::filter::Pipeline<'repo>,
    /// The index the filter pipeline was configured against.
    filter_index: gix::index::File,
    workdir: PathBuf,
    /// Repo-relative directory of the current working directory, `""` or `"…/"`.
    cwd_prefix: String,
    fs: gix::fs::Capabilities,
    stat_opts: StatOptions,
    opts: Opts,
    /// `--temp` names for stages 0..=3, empty when nothing was written.
    topath: [String; 4],
    /// Stat updates to fold back into the index for `-u`.
    stat_updates: Vec<(usize, Stat)>,
    out: Vec<u8>,
}

/// `git checkout-index` — copy files from the index to the working tree.
///
/// With no paths and no `-a` this does nothing and exits 0, which is git's
/// documented "no arguments means no work" contract for scripted use.
pub fn checkout_index(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts::default();
    let mut files: Vec<String> = Vec::new();

    let mut i = 0;
    let mut end_of_opts = false;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts {
            files.push(a.to_string());
            i += 1;
            continue;
        }
        if a == "--" {
            end_of_opts = true;
            i += 1;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            // Pull `--opt <value>` from the next argument when no `=` was used.
            let mut value = |i: &mut usize| -> Result<String> {
                match inline {
                    Some(v) => Ok(v.to_string()),
                    None => {
                        *i += 1;
                        args.get(*i)
                            .cloned()
                            .ok_or_else(|| anyhow::anyhow!("option `{name}' requires a value"))
                    }
                }
            };
            match name {
                "all" => opts.all = true,
                "no-all" => opts.all = false,
                "force" => opts.force = true,
                "no-force" => opts.force = false,
                "quiet" => opts.quiet = true,
                "no-quiet" => opts.quiet = false,
                "no-create" => opts.not_new = true,
                "create" => opts.not_new = false,
                "index" => opts.refresh_cache = true,
                "no-index" => opts.refresh_cache = false,
                "stdin" => opts.from_stdin = true,
                "no-stdin" => opts.from_stdin = false,
                "temp" => opts.to_tempfile = true,
                "no-temp" => opts.to_tempfile = false,
                "ignore-skip-worktree-bits" => opts.ignore_skip_worktree = true,
                "no-ignore-skip-worktree-bits" => opts.ignore_skip_worktree = false,
                "prefix" => opts.base_dir = value(&mut i)?,
                "no-prefix" => opts.base_dir.clear(),
                "stage" => {
                    let v = value(&mut i)?;
                    if v == "all" {
                        opts.to_tempfile = true;
                        opts.stage = CHECKOUT_ALL;
                    } else {
                        // git inspects only the first byte of the argument.
                        match v.as_bytes().first() {
                            Some(c @ b'1'..=b'3') => opts.stage = u32::from(c - b'0'),
                            _ => return die("stage should be between 1 and 3 or all"),
                        }
                    }
                }
                _ => return unknown_option(&format!("unknown option `{name}'")),
            }
            i += 1;
            continue;
        }
        if a.len() > 1 && a.starts_with('-') {
            for c in a[1..].chars() {
                match c {
                    'a' => opts.all = true,
                    'f' => opts.force = true,
                    'q' => opts.quiet = true,
                    'n' => opts.not_new = true,
                    'u' => opts.refresh_cache = true,
                    'z' => opts.nul_term = true,
                    _ => return unknown_option(&format!("unknown switch `{c}'")),
                }
            }
            i += 1;
            continue;
        }
        files.push(a.to_string());
        i += 1;
    }

    if opts.all && opts.from_stdin {
        return die("git checkout-index: don't mix '--all' and '--stdin'");
    }
    if opts.all && !files.is_empty() {
        return die("git checkout-index: don't mix '--all' and explicit filenames");
    }
    if opts.from_stdin && !files.is_empty() {
        return die("git checkout-index: don't mix '--stdin' and explicit filenames");
    }
    // git only holds the index lock when neither --prefix nor --temp is in play,
    // so `-u` is silently inert in those modes.
    if !opts.base_dir.is_empty() || opts.to_tempfile {
        opts.refresh_cache = false;
    }

    let repo = gix::discover(".")?;
    let Some(workdir) = repo.workdir().map(Path::to_path_buf) else {
        return die("this operation must be run in a work tree");
    };
    let cwd_prefix = match repo.prefix()? {
        Some(p) if !p.as_os_str().is_empty() => {
            let mut s = gix::path::into_bstr(p).to_str_lossy().into_owned();
            s.push('/');
            s
        }
        _ => String::new(),
    };

    let mut index = repo.open_index()?;
    let stat_opts = repo.stat_options()?;
    let index_timestamp = index.timestamp();
    let ents: Vec<Ent> = index
        .entries()
        .iter()
        .map(|e| Ent {
            path: e.path(&index).to_owned(),
            mode: e.mode,
            id: e.id,
            stat: e.stat,
            stage: e.stage_raw(),
            skip_worktree: e.flags.contains(gix::index::entry::Flags::SKIP_WORKTREE),
            racy: e.stat.is_racy(index_timestamp, stat_opts),
        })
        .collect();
    if let Some(e) = ents.iter().find(|e| e.mode == Mode::DIR) {
        bail!(
            "unsupported: sparse-directory index entry {:?} (cone-mode sparse checkout is not ported)",
            e.path.to_string()
        );
    }

    let cache = repo.attributes_only(
        &index,
        gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
    )?;
    let mut ctx = Ctx {
        pipeline: gix::filter::Pipeline::new(&repo, cache.detach())?,
        filter_index: repo.open_index()?,
        repo: &repo,
        workdir,
        cwd_prefix,
        fs: repo.filesystem_options()?,
        stat_opts,
        opts,
        topath: Default::default(),
        stat_updates: Vec::new(),
        out: Vec::new(),
    };

    let mut errs = false;
    if ctx.opts.all {
        errs |= checkout_all(&mut ctx, &ents)?;
    } else {
        let names: Vec<BString> = if ctx.opts.from_stdin {
            match read_stdin_paths(ctx.opts.nul_term)? {
                Some(names) => names,
                None => return die("line is badly quoted"),
            }
        } else {
            files.iter().map(|f| BString::from(f.as_str())).collect()
        };
        for raw in names {
            let Some(name) = prefix_path(&ctx.workdir, &ctx.cwd_prefix, raw.as_bstr()) else {
                std::io::stdout().write_all(&ctx.out)?;
                let cwd = std::env::current_dir()?;
                return die(&format!(
                    "'{}' is outside repository at '{}'",
                    raw.to_str_lossy(),
                    cwd.display()
                ));
            };
            errs |= checkout_file(&mut ctx, &ents, name.as_bstr())?;
        }
    }

    std::io::stdout().write_all(&ctx.out)?;

    if ctx.opts.refresh_cache && !ctx.stat_updates.is_empty() {
        let entries = index.entries_mut();
        for (idx, stat) in &ctx.stat_updates {
            entries[*idx].stat = *stat;
        }
        index.write(gix::index::write::Options::default())?;
    }

    Ok(if errs {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// git's `die()`: `fatal: <msg>` on stderr, exit 128.
fn die(msg: &str) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// git's parse-options failure: the error, then the usage block, exit 129.
fn unknown_option(msg: &str) -> Result<ExitCode> {
    eprint!("error: {msg}\n{USAGE}");
    Ok(ExitCode::from(129))
}

/// git's `checkout_all()`: walk the whole index, honouring the skip-worktree
/// bit, the requested stage, and the current directory as a path limiter.
fn checkout_all(ctx: &mut Ctx<'_>, ents: &[Ent]) -> Result<bool> {
    let mut errs = false;
    let mut last: Option<&BString> = None;

    for (idx, ent) in ents.iter().enumerate() {
        if !ctx.opts.ignore_skip_worktree && ent.skip_worktree {
            continue;
        }
        if !stage_selected(ent.stage, ctx.opts.stage) {
            continue;
        }
        if !ctx.cwd_prefix.is_empty() && !ent.path.starts_with_str(&ctx.cwd_prefix) {
            continue;
        }
        // A `--temp` listing line covers all stages of one path, so it is
        // flushed when the path changes.
        if ctx.opts.to_tempfile {
            if let Some(prev) = last {
                if prev != &ent.path {
                    let prev = prev.clone();
                    write_tempfile_record(ctx, prev.as_bstr());
                }
            }
        }
        errs |= !checkout_entry(ctx, ents, idx)?;
        last = Some(&ent.path);
    }

    if ctx.opts.to_tempfile {
        if let Some(prev) = last {
            let prev = prev.clone();
            write_tempfile_record(ctx, prev.as_bstr());
        }
    }
    Ok(errs)
}

/// git's `checkout_file()`: check out every stage of `name` that the requested
/// `--stage` selects, and diagnose the reason when nothing was checked out.
fn checkout_file(ctx: &mut Ctx<'_>, ents: &[Ent], name: &BStr) -> Result<bool> {
    let mut pos = ents.partition_point(|e| e.path.as_bstr() < name);
    let mut has_same_name = false;
    let mut is_skipped = true;
    let mut did_checkout = false;
    let mut errs = false;

    while pos < ents.len() {
        let ent = &ents[pos];
        if ent.path.as_bstr() != name {
            break;
        }
        has_same_name = true;
        pos += 1;
        if !ctx.opts.ignore_skip_worktree && ent.skip_worktree {
            break;
        }
        is_skipped = false;
        if !stage_selected(ent.stage, ctx.opts.stage) {
            continue;
        }
        did_checkout = true;
        errs |= !checkout_entry(ctx, ents, pos - 1)?;
    }

    if did_checkout {
        if ctx.opts.to_tempfile {
            write_tempfile_record(ctx, name);
        }
        return Ok(errs);
    }
    // Finding only a stage-0 entry while asking for all stages is not an error.
    if has_same_name && ctx.opts.stage == CHECKOUT_ALL {
        return Ok(false);
    }
    if !ctx.opts.quiet {
        let reason = if !has_same_name {
            "is not in the cache".to_string()
        } else if is_skipped {
            "has skip-worktree enabled".to_string()
        } else if ctx.opts.stage != 0 {
            format!("does not exist at stage {}", ctx.opts.stage)
        } else {
            "is unmerged".to_string()
        };
        eprintln!("git checkout-index: {} {reason}", name.to_str_lossy());
    }
    Ok(true)
}

/// git's stage filter: keep an entry whose stage is the requested one, or any
/// conflicted stage when `--stage=all` was given.
fn stage_selected(entry_stage: u32, requested: u32) -> bool {
    entry_stage == requested || (requested == CHECKOUT_ALL && entry_stage != 0)
}

/// git's `checkout_entry_ca()`: decide whether the destination has to be
/// (re)written, then write it. Returns `false` when the entry failed, having
/// already reported why.
fn checkout_entry(ctx: &mut Ctx<'_>, ents: &[Ent], idx: usize) -> Result<bool> {
    // `--temp` never inspects the destination: it always creates a new file.
    if ctx.opts.to_tempfile {
        return write_entry(ctx, ents, idx, true);
    }

    let dest = dest_path(ctx, &ents[idx].path);
    match gix::index::fs::Metadata::from_path_no_follow(&dest) {
        Ok(md) => {
            if !entry_changed(ctx, &ents[idx], &md, &dest)? {
                return Ok(true);
            }
            if !ctx.opts.force {
                if !ctx.opts.quiet {
                    eprintln!(
                        "{} already exists, no checkout",
                        display_path(ctx, &ents[idx].path)
                    );
                }
                return Ok(false);
            }
            // git's `unlink_entry()`: drop what is in the way before rewriting.
            if md.is_dir() {
                let _ = std::fs::remove_dir(&dest);
            } else {
                let _ = std::fs::remove_file(&dest);
            }
        }
        // Nothing there: `-n` means "refresh only", so skip creating it.
        Err(_) if ctx.opts.not_new => return Ok(true),
        Err(_) => {}
    }

    let ok = write_entry(ctx, ents, idx, false)?;
    if ok && ctx.opts.refresh_cache {
        if let Ok(md) = gix::index::fs::Metadata::from_path_no_follow(&dest) {
            if let Ok(stat) = Stat::from_fs(&md) {
                ctx.stat_updates.push((idx, stat));
            }
        }
    }
    Ok(ok)
}

/// git's `ie_match_stat()` restricted to what `checkout-index` asks of it:
/// mode/type change, the configured stat comparison, the racily-smudged
/// zero-size rule, and the racy-timestamp content fallback.
fn entry_changed(
    ctx: &mut Ctx<'_>,
    ent: &Ent,
    md: &gix::index::fs::Metadata,
    dest: &Path,
) -> Result<bool> {
    if ent
        .mode
        .change_to_match_fs(md, ctx.fs.symlink, ctx.fs.executable_bit)
        .is_some()
    {
        return Ok(true);
    }
    // A gitlink is only ever compared by type; its HEAD is not consulted here.
    if ent.mode == Mode::COMMIT {
        return Ok(false);
    }

    let fs_stat = Stat::from_fs(md)?;
    if !fs_stat.matches(&ent.stat, ctx.stat_opts) {
        return Ok(true);
    }
    // A zero recorded size marks an entry that was smudged by a previous racy
    // detection; only the empty blob may legitimately have one.
    if ent.stat.size == 0 && !ent.id.is_empty_blob() {
        return Ok(true);
    }
    if !ent.racy {
        return Ok(false);
    }
    // Racy: the stat cannot be trusted, so compare content like git's
    // `ce_modified_check_fs()` does.
    Ok(worktree_blob_id(ctx, ent, md, dest)? != ent.id)
}

/// The blob id the file at `dest` would have, with the clean pipeline applied to
/// regular files and a symlink's target taken verbatim.
fn worktree_blob_id(
    ctx: &mut Ctx<'_>,
    ent: &Ent,
    md: &gix::index::fs::Metadata,
    dest: &Path,
) -> Result<ObjectId> {
    let data = if md.is_symlink() {
        gix::path::into_bstr(std::fs::read_link(dest)?).into_owned()
    } else {
        let rela = gix::path::from_bstr(ent.path.as_bstr()).into_owned();
        let file = std::fs::File::open(dest)?;
        let mut converted = ctx
            .pipeline
            .convert_to_git(file, &rela, &ctx.filter_index)?;
        let mut buf = Vec::new();
        std::io::copy(&mut converted, &mut buf)?;
        buf.into()
    };
    Ok(gix::objs::compute_hash(
        ctx.repo.object_hash(),
        gix::objs::Kind::Blob,
        &data,
    )?)
}

/// git's `write_entry()`: materialise one index entry, either at its worktree
/// destination or, for `--temp`, as a fresh temporary file at the worktree root.
fn write_entry(ctx: &mut Ctx<'_>, ents: &[Ent], idx: usize, to_tempfile: bool) -> Result<bool> {
    let ent = &ents[idx];
    let dest = dest_path(ctx, &ent.path);

    match ent.mode {
        Mode::SYMLINK => {
            let target = ctx.repo.find_object(ent.id)?.data.clone();
            // Without `--temp` and with real symlink support, the blob content is
            // the link target; otherwise the content lands in a regular file.
            if !to_tempfile && ctx.fs.symlink {
                if let Err(e) = create_symlink(&dest, &target) {
                    eprintln!(
                        "error: unable to create symlink {}: {e}",
                        display_path(ctx, &ent.path)
                    );
                    return Ok(false);
                }
                return Ok(true);
            }
            let name = ".merge_link_XXXXXX";
            write_regular(ctx, ents, idx, &dest, &target, to_tempfile, name)
        }
        Mode::FILE | Mode::FILE_EXECUTABLE => {
            let blob = ctx.repo.find_object(ent.id)?.data.clone();
            let rela = ent.path.clone();
            let mut converted = ctx.pipeline.convert_to_worktree(
                &blob,
                rela.as_bstr(),
                gix::filter::plumbing::driver::apply::Delay::Forbid,
            )?;
            let mut data = Vec::new();
            std::io::copy(&mut converted, &mut data)?;
            drop(converted);
            let name = ".merge_file_XXXXXX";
            write_regular(ctx, ents, idx, &dest, &data, to_tempfile, name)
        }
        Mode::COMMIT => {
            if to_tempfile {
                eprintln!(
                    "error: cannot create temporary submodule {}",
                    ent.path.to_str_lossy()
                );
                return Ok(false);
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if std::fs::create_dir(&dest).is_err() {
                eprintln!(
                    "error: cannot create submodule directory {}",
                    display_path(ctx, &ent.path)
                );
                return Ok(false);
            }
            Ok(true)
        }
        _ => {
            eprintln!(
                "error: unknown file mode for {} in index",
                display_path(ctx, &ent.path)
            );
            Ok(false)
        }
    }
}

/// Create a regular file holding `data`, either at `dest` or, for `--temp`, at a
/// fresh `<template>` name in the worktree root whose name is recorded for the
/// listing. Temporary files always get mode 0666; real checkouts get 0777 when
/// the entry is executable, so the process umask decides the final bits.
fn write_regular(
    ctx: &mut Ctx<'_>,
    ents: &[Ent],
    idx: usize,
    dest: &Path,
    data: &[u8],
    to_tempfile: bool,
    template: &str,
) -> Result<bool> {
    if to_tempfile {
        match create_tempfile(&ctx.workdir, template, data) {
            Ok(name) => {
                ctx.topath[ents[idx].stage as usize] = name;
                Ok(true)
            }
            Err(e) => {
                eprintln!("error: unable to create temporary file: {e}");
                Ok(false)
            }
        }
    } else {
        let exec = ents[idx].mode == Mode::FILE_EXECUTABLE;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match create_file(dest, exec, data) {
            Ok(()) => Ok(true),
            Err(e) => {
                eprintln!(
                    "error: unable to create file {}: {e}",
                    display_path(ctx, &ents[idx].path)
                );
                Ok(false)
            }
        }
    }
}

/// git's `create_file()`: `O_CREAT|O_EXCL` with 0777 or 0666 so the umask applies.
#[cfg(unix)]
fn create_file(path: &Path, executable: bool, data: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(if executable { 0o777 } else { 0o666 })
        .open(path)?;
    f.write_all(data)
}

#[cfg(not(unix))]
fn create_file(path: &Path, _executable: bool, data: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(data)
}

#[cfg(unix)]
fn create_symlink(path: &Path, target: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::os::unix::fs::symlink(gix::path::from_byte_slice(target), path)
}

#[cfg(not(unix))]
fn create_symlink(path: &Path, target: &[u8]) -> std::io::Result<()> {
    create_file(path, false, target)
}

/// git's `git_mkstemp_mode()` on a `XXXXXX` template: replace the six trailing
/// `X`s with characters from git's alphabet until an exclusive create succeeds.
/// The file is created in `dir` and its bare name is returned.
fn create_tempfile(dir: &Path, template: &str, data: &[u8]) -> std::io::Result<String> {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let stem = template.trim_end_matches('X');

    let mut seed = seed();
    for _ in 0..1000 {
        let mut name = String::with_capacity(template.len());
        name.push_str(stem);
        for _ in 0..template.len() - stem.len() {
            // xorshift64: enough spread for a filename, no dependency needed.
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            name.push(LETTERS[(seed % LETTERS.len() as u64) as usize] as char);
        }
        match create_file(&dir.join(&name), false, data) {
            Ok(()) => return Ok(name),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not find an unused temporary file name",
    ))
}

/// A per-call seed for the temporary-name generator.
fn seed() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let n = nanos
        ^ (u64::from(std::process::id()) << 32)
        ^ COUNTER.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
    if n == 0 {
        0x2545_f491_4f6c_dd1d
    } else {
        n
    }
}

/// git's `write_tempfile_record()`: emit the `tempname TAB path RS` line (or the
/// three-name `--stage=all` form) and clear the recorded names.
fn write_tempfile_record(ctx: &mut Ctx<'_>, name: &BStr) {
    let mut have_tempname = false;
    let mut line: Vec<u8> = Vec::new();

    if ctx.opts.stage == CHECKOUT_ALL {
        have_tempname = ctx.topath[1..4].iter().any(|t| !t.is_empty());
        if have_tempname {
            for i in 1..4 {
                if i > 1 {
                    line.push(b' ');
                }
                if ctx.topath[i].is_empty() {
                    line.push(b'.');
                } else {
                    line.extend_from_slice(ctx.topath[i].as_bytes());
                }
            }
        }
    } else if !ctx.topath[ctx.opts.stage as usize].is_empty() {
        have_tempname = true;
        line.extend_from_slice(ctx.topath[ctx.opts.stage as usize].as_bytes());
    }

    if have_tempname {
        line.push(b'\t');
        let rel = relative_path(name, &ctx.cwd_prefix);
        line.extend_from_slice(quote_c_style(&rel).as_bytes());
        line.push(if ctx.opts.nul_term { b'\0' } else { b'\n' });
        ctx.out.extend_from_slice(&line);
    }
    ctx.topath = Default::default();
}

/// Where an entry is written: the worktree root, plus `--prefix`, plus the
/// repo-relative entry path.
fn dest_path(ctx: &Ctx<'_>, path: &BString) -> PathBuf {
    let mut joined = BString::from(ctx.opts.base_dir.as_str());
    joined.extend_from_slice(path);
    ctx.workdir.join(gix::path::from_bstr(joined.as_bstr()))
}

/// The path git names in its diagnostics: `--prefix` plus the entry path, always
/// relative to the worktree root (git runs with that as its current directory).
fn display_path(ctx: &Ctx<'_>, path: &BString) -> String {
    format!("{}{}", ctx.opts.base_dir, path.to_str_lossy())
}

/// git's `prefix_path()`: resolve a command-line path against the repo-relative
/// current directory. An absolute path is accepted only inside the worktree.
/// `None` when the result escapes the repository, which git reports as fatal.
fn prefix_path(workdir: &Path, cwd_prefix: &str, arg: &BStr) -> Option<BString> {
    let arg = arg.to_str_lossy();
    let joined = if arg.starts_with('/') {
        let root = gix::path::into_bstr(workdir).to_str_lossy().into_owned();
        let root = root.trim_end_matches('/');
        let rest = arg.strip_prefix(root)?;
        rest.trim_start_matches('/').to_string()
    } else {
        format!("{cwd_prefix}{arg}")
    };

    let mut parts: Vec<&str> = Vec::new();
    for c in joined.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(BString::from(parts.join("/")))
}

/// git's `relative_path()`: express the repo-relative `path` relative to the
/// repo-relative directory `prefix`, using `../` to climb where needed.
fn relative_path(path: &BStr, prefix: &str) -> BString {
    if prefix.is_empty() {
        return path.to_owned();
    }
    let path_s = path.to_str_lossy();
    let path_parts: Vec<&str> = path_s.split('/').collect();
    let prefix_parts: Vec<&str> = prefix.trim_end_matches('/').split('/').collect();

    let common = path_parts
        .iter()
        .zip(prefix_parts.iter())
        // The last path component is the file name, never a directory to share.
        .take(path_parts.len().saturating_sub(1))
        .take_while(|(a, b)| a == b)
        .count();

    let mut out = String::new();
    for _ in common..prefix_parts.len() {
        out.push_str("../");
    }
    out.push_str(&path_parts[common..].join("/"));
    BString::from(out)
}

/// git's `quote_c_style()` with `nodq = 0`: quote and escape only when the path
/// contains a control byte, a quote, a backslash, or any byte >= 0x80.
fn quote_c_style(bytes: &[u8]) -> String {
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        // Every byte is printable ASCII here, so this cannot lose information.
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

/// Read the `--stdin` path list. Without `-z` a trailing CR is stripped with the
/// LF and a leading `"` marks a C-quoted record; `None` reports one that git
/// would reject as badly quoted.
fn read_stdin_paths(nul_term: bool) -> Result<Option<Vec<BString>>> {
    use std::io::Read;
    let mut raw = Vec::new();
    std::io::stdin().read_to_end(&mut raw)?;

    let sep = if nul_term { b'\0' } else { b'\n' };
    let mut out = Vec::new();
    for record in raw.split(|&b| b == sep) {
        // A trailing separator yields one empty tail record, which git's
        // line reader never returns.
        if record.is_empty() {
            continue;
        }
        if nul_term {
            out.push(BString::from(record));
            continue;
        }
        let record = record.strip_suffix(b"\r").unwrap_or(record);
        if record.first() == Some(&b'"') {
            match unquote_c_style(record) {
                Some(decoded) => out.push(decoded),
                None => return Ok(None),
            }
        } else {
            out.push(BString::from(record));
        }
    }
    Ok(Some(out))
}

/// git's `unquote_c_style()`: decode a `"`-delimited, backslash-escaped record.
fn unquote_c_style(input: &[u8]) -> Option<BString> {
    let mut out = Vec::new();
    let mut it = input.strip_prefix(b"\"")?.iter().copied();
    loop {
        let b = it.next()?;
        match b {
            b'"' => return Some(BString::from(out)),
            b'\\' => {
                let e = it.next()?;
                match e {
                    b'a' => out.push(0x07),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'v' => out.push(0x0b),
                    b'"' | b'\\' => out.push(e),
                    b'0'..=b'7' => {
                        // One to three octal digits, as git's decoder accepts.
                        let mut v = u32::from(e - b'0');
                        let mut peek = it.clone();
                        for _ in 0..2 {
                            match peek.next() {
                                Some(d @ b'0'..=b'7') => {
                                    v = v * 8 + u32::from(d - b'0');
                                    it.next();
                                }
                                _ => break,
                            }
                        }
                        out.push(v as u8);
                    }
                    _ => return None,
                }
            }
            b => out.push(b),
        }
    }
}
