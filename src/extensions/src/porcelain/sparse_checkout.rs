use anyhow::{anyhow, bail, Result};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BString, ByteSlice};
use gix::config::{File as ConfigFile, Source};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode};

/// `git sparse-checkout` — restrict the worktree to a subset of tracked files.
///
/// Cone mode only (git's default). The pattern file
/// (`<git-dir>/info/sparse-checkout`) is generated in git's exact cone layout —
/// the `/*` + `!/*/` root pair, then one `/<parent>/` + `!/<parent>/*/` pair per
/// ancestor directory (sorted), then one `/<dir>/` line per recursive directory
/// (sorted) — so a file written here is byte-identical to stock git's.
///
/// Applying sparsity walks the index: entries outside the cone get the
/// `SKIP_WORKTREE` bit (and are deleted from disk, pruning directories that
/// become empty), entries inside get it cleared and are materialised through
/// gitoxide's worktree checkout. Files with local modifications are left alone
/// and keep their bit clear, matching git's refusal to sparsify dirty paths.
/// Config is written where git writes it: `core.sparseCheckout` /
/// `core.sparseCheckoutCone` into `<git-dir>/config.worktree`, with
/// `extensions.worktreeConfig=true` in the repository-local config.
///
/// Supported subcommands and flags:
///   * `list`                                   — cone dirs (or raw patterns in
///                                                a non-cone worktree)
///   * `set [--cone] [--no-sparse-index] [--stdin] <dir>...`
///   * `add [--stdin] <dir>...`
///   * `init [--cone] [--no-sparse-index]`
///   * `reapply`
///   * `disable`
///   * `check-rules [-z] [--rules-file <file>]`
///
/// Faithfully unsupported (each `bail!`s or reports git's own fatal rather than
/// producing a divergent worktree): `--no-cone` for anything that has to *match*
/// patterns (`set`/`add`/`reapply`/`check-rules`) — non-cone sparse patterns
/// need a gitignore-semantics matcher this port does not wire up; the
/// `--sparse-index` sparse-directory index extension, which the vendored
/// `gix-index` cannot write; and the `clean` subcommand.
///
/// Paths are matched as lossy UTF-8, so a tracked path with invalid UTF-8 bytes
/// may be classified differently than git would classify it.
pub fn sparse_checkout(args: &[String]) -> Result<ExitCode> {
    let Some(sub) = args.get(1) else {
        eprint!("error: need a subcommand\n{USAGE}\n");
        return Ok(ExitCode::from(129));
    };
    let rest = &args[2..];

    match sub.as_str() {
        "list" => cmd_list(rest),
        "set" => cmd_set(rest, false),
        "add" => cmd_set(rest, true),
        "init" => cmd_init(rest),
        "reapply" => cmd_reapply(rest),
        "disable" => cmd_disable(rest),
        "check-rules" => cmd_check_rules(rest),
        "clean" => bail!(
            "the 'clean' subcommand is not supported (ported: list, set, add, init, reapply, disable, check-rules)"
        ),
        other => {
            eprint!("error: unknown subcommand: `{other}'\n{USAGE}\n");
            Ok(ExitCode::from(129))
        }
    }
}

const USAGE: &str = "usage: git sparse-checkout (init | list | set | add | reapply | disable | check-rules | clean) [<options>]\n";

// --- subcommands -----------------------------------------------------------

fn cmd_list(args: &[String]) -> Result<ExitCode> {
    reject_unknown(args, &[])?;
    let repo = gix::discover(".")?;
    if !is_sparse(&repo)? {
        eprintln!("fatal: this worktree is not sparse");
        return Ok(ExitCode::from(128));
    }

    let lines = read_pattern_file(&repo)?;
    let mut out = String::new();
    if is_cone(&repo)? {
        for d in cone_dirs(&lines) {
            out.push_str(&quote_path(d.as_bytes()));
            out.push('\n');
        }
    } else {
        // Non-cone worktrees list the raw patterns verbatim.
        for l in &lines {
            out.push_str(l);
            out.push('\n');
        }
    }
    print!("{out}");
    Ok(ExitCode::SUCCESS)
}

/// `set` (`add == true` merges into the existing cone instead of replacing it).
fn cmd_set(args: &[String], add: bool) -> Result<ExitCode> {
    let mut stdin = false;
    let mut positional: Vec<&str> = Vec::new();
    for a in args {
        match a.as_str() {
            "--stdin" => stdin = true,
            "--cone" => {}                // the default
            "--no-sparse-index" => {}     // the default: we never write a sparse index
            "--no-cone" => bail!("--no-cone is not supported (non-cone pattern matching is not ported)"),
            "--sparse-index" => bail!("--sparse-index is not supported (sparse-directory index entries cannot be written)"),
            _ if a.starts_with('-') => bail!("unsupported flag {a:?} (ported: --cone, --stdin, --no-sparse-index)"),
            _ => positional.push(a.as_str()),
        }
    }

    let mut inputs: Vec<String> = Vec::new();
    if stdin {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        inputs.extend(buf.lines().map(str::to_owned));
    }
    inputs.extend(positional.into_iter().map(str::to_owned));

    let repo = gix::discover(".")?;
    if add && !is_sparse(&repo)? {
        eprintln!("fatal: no sparse-checkout to add to");
        return Ok(ExitCode::from(128));
    }

    let mut dirs: BTreeSet<String> = if add {
        cone_dirs(&read_pattern_file(&repo)?)
    } else {
        BTreeSet::new()
    };
    for raw in &inputs {
        match normalize_dir(raw)? {
            // An empty argument (e.g. a blank `--stdin` line) names no directory.
            Some(d) if d.is_empty() => {}
            Some(d) => {
                dirs.insert(d);
            }
            None => {
                eprintln!("fatal: specify directories rather than patterns (no leading slash)");
                return Ok(ExitCode::from(128));
            }
        }
    }

    let cone = Cone::new(dedup_nested(dirs));
    write_pattern_file(&repo, &cone)?;
    enable_config(&repo)?;
    apply(&repo, Some(&cone))?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_init(args: &[String]) -> Result<ExitCode> {
    for a in args {
        match a.as_str() {
            "--cone" | "--no-sparse-index" => {}
            "--no-cone" => bail!("--no-cone is not supported (non-cone pattern matching is not ported)"),
            "--sparse-index" => bail!("--sparse-index is not supported (sparse-directory index entries cannot be written)"),
            _ => bail!("unsupported flag {a:?} (ported: --cone, --no-sparse-index)"),
        }
    }
    let repo = gix::discover(".")?;

    // `init` keeps an existing pattern file (that is how a `disable`d sparsity
    // is restored); only a missing one is seeded with the empty cone.
    let cone = if pattern_path(&repo).exists() {
        Cone::new(cone_dirs(&read_pattern_file(&repo)?))
    } else {
        let c = Cone::new(BTreeSet::new());
        write_pattern_file(&repo, &c)?;
        c
    };
    enable_config(&repo)?;
    apply(&repo, Some(&cone))?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_reapply(args: &[String]) -> Result<ExitCode> {
    for a in args {
        match a.as_str() {
            "--cone" | "--no-sparse-index" => {}
            "--no-cone" => bail!("--no-cone is not supported (non-cone pattern matching is not ported)"),
            "--sparse-index" => bail!("--sparse-index is not supported (sparse-directory index entries cannot be written)"),
            _ => bail!("unsupported flag {a:?} (ported: --cone, --no-sparse-index)"),
        }
    }
    let repo = gix::discover(".")?;
    if !is_sparse(&repo)? {
        eprintln!("fatal: this worktree is not sparse");
        return Ok(ExitCode::from(128));
    }
    if !is_cone(&repo)? {
        bail!("reapply in a non-cone worktree is not supported (non-cone pattern matching is not ported)");
    }
    let cone = Cone::new(cone_dirs(&read_pattern_file(&repo)?));
    apply(&repo, Some(&cone))?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_disable(args: &[String]) -> Result<ExitCode> {
    reject_unknown(args, &[])?;
    let repo = gix::discover(".")?;
    // git leaves the pattern file in place so a later `init` can restore it.
    apply(&repo, None)?;
    disable_config(&repo)?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_check_rules(args: &[String]) -> Result<ExitCode> {
    let mut nul = false;
    let mut rules_file: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-z" => nul = true,
            "--cone" => {}
            "--rules-file" => {
                rules_file = Some(PathBuf::from(
                    it.next().ok_or_else(|| anyhow!("--rules-file requires a value"))?,
                ));
            }
            _ if a.starts_with("--rules-file=") => {
                rules_file = Some(PathBuf::from(&a["--rules-file=".len()..]));
            }
            "--no-cone" => bail!("--no-cone is not supported (non-cone pattern matching is not ported)"),
            _ => bail!("unsupported flag {a:?} (ported: -z, --cone, --rules-file)"),
        }
    }

    let cone = match &rules_file {
        // `--rules-file` holds a newline-delimited *directory* list in cone mode.
        Some(p) => {
            let text = std::fs::read_to_string(p)?;
            let mut dirs = BTreeSet::new();
            for line in text.lines() {
                if let Some(d) = normalize_dir(line)? {
                    if !d.is_empty() {
                        dirs.insert(d);
                    }
                }
            }
            Cone::new(dedup_nested(dirs))
        }
        None => {
            let repo = gix::discover(".")?;
            if !pattern_path(&repo).exists() {
                eprintln!("fatal: this worktree is not sparse");
                return Ok(ExitCode::from(128));
            }
            if !is_cone(&repo)? {
                bail!("check-rules in a non-cone worktree is not supported (non-cone pattern matching is not ported)");
            }
            Cone::new(cone_dirs(&read_pattern_file(&repo)?))
        }
    };

    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let sep = if nul { b'\0' } else { b'\n' };

    let mut out = Vec::new();
    for raw in input.split(|&b| b == sep) {
        if raw.is_empty() {
            continue;
        }
        // Without -z, a leading double quote marks a C-style quoted path.
        let path = if nul {
            BString::from(raw)
        } else {
            unquote_c(raw)?
        };
        if cone.matches(&path.to_str_lossy()) {
            if nul {
                out.extend_from_slice(&path);
                out.push(0);
            } else {
                out.extend_from_slice(quote_path(&path).as_bytes());
                out.push(b'\n');
            }
        }
    }
    std::io::stdout().write_all(&out)?;
    Ok(ExitCode::SUCCESS)
}

fn reject_unknown(args: &[String], allowed: &[&str]) -> Result<()> {
    for a in args {
        if !allowed.contains(&a.as_str()) {
            bail!("unsupported flag {a:?}");
        }
    }
    Ok(())
}

// --- cone model ------------------------------------------------------------

/// A cone-mode sparsity definition: the recursive directories plus every
/// ancestor of them (the "parent" directories, which contribute only their
/// immediate files).
struct Cone {
    /// Recursive directories, repo-relative, no leading or trailing slash.
    recursive: BTreeSet<String>,
    /// Strict ancestors of `recursive`, plus the repository root as `""`.
    parents: BTreeSet<String>,
}

impl Cone {
    fn new(recursive: BTreeSet<String>) -> Self {
        let mut parents = BTreeSet::new();
        parents.insert(String::new());
        for d in &recursive {
            let mut acc = String::new();
            for comp in d.split('/') {
                if acc.is_empty() {
                    acc.push_str(comp);
                } else {
                    acc.push('/');
                    acc.push_str(comp);
                }
                if acc != *d {
                    parents.insert(acc.clone());
                }
            }
        }
        Cone { recursive, parents }
    }

    /// Whether the tracked file `path` is inside the cone: it either sits
    /// directly in a parent directory (or the root), or lives at any depth
    /// under a recursive directory.
    fn matches(&self, path: &str) -> bool {
        let dir = match path.rfind('/') {
            Some(i) => &path[..i],
            None => "",
        };
        if self.parents.contains(dir) {
            return true;
        }
        self.recursive
            .iter()
            .any(|d| dir == d || dir.starts_with(&format!("{d}/")))
    }
}

/// Normalize one `set`/`add` argument into a repo-relative directory.
///
/// Returns `Ok(None)` for a leading-slash argument, which git rejects as a
/// pattern rather than a directory. An argument starting with `"` is a C-style
/// quoted path. Empty input (e.g. a blank `--stdin` line) is dropped.
fn normalize_dir(raw: &str) -> Result<Option<String>> {
    if raw.starts_with('/') {
        return Ok(None);
    }
    let unquoted = unquote_c(raw.as_bytes())?;
    let s = unquoted.to_str_lossy().into_owned();
    if s.starts_with('/') {
        return Ok(None);
    }
    Ok(Some(s.trim_end_matches('/').to_owned()))
}

/// Drop any directory whose ancestor is already present — a recursive parent
/// already covers it, and git's cone writer collapses them the same way
/// (`set a a/b` writes only `/a/`).
fn dedup_nested(dirs: BTreeSet<String>) -> BTreeSet<String> {
    dirs.iter()
        .filter(|d| {
            !dirs
                .iter()
                .any(|o| o.as_str() != d.as_str() && d.starts_with(&format!("{o}/")))
        })
        .cloned()
        .collect()
}

// --- pattern file ----------------------------------------------------------

fn pattern_path(repo: &gix::Repository) -> PathBuf {
    repo.git_dir().join("info").join("sparse-checkout")
}

fn read_pattern_file(repo: &gix::Repository) -> Result<Vec<String>> {
    let path = pattern_path(repo);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    Ok(text.lines().map(str::to_owned).collect())
}

/// Recover the recursive directory set from a cone pattern file.
///
/// Positive `/<dir>/` lines name both parents and recursive directories; the
/// parents are exactly those that also carry a `!/<dir>/*/` exclusion (and the
/// root, `!/*/`). The difference is the recursive set.
fn cone_dirs(lines: &[String]) -> BTreeSet<String> {
    let mut positive = BTreeSet::new();
    let mut parent = BTreeSet::new();
    for l in lines {
        let l = l.trim();
        if l == "/*" || l.is_empty() {
            continue;
        }
        if l == "!/*/" {
            parent.insert(String::new());
        } else if let Some(inner) = l.strip_prefix("!/").and_then(|r| r.strip_suffix("/*/")) {
            parent.insert(inner.to_owned());
        } else if let Some(inner) = l.strip_prefix('/').and_then(|r| r.strip_suffix('/')) {
            positive.insert(inner.to_owned());
        }
    }
    positive.difference(&parent).cloned().collect()
}

/// Write the pattern file in git's cone layout.
fn write_pattern_file(repo: &gix::Repository, cone: &Cone) -> Result<()> {
    let mut out = String::from("/*\n!/*/\n");
    for p in &cone.parents {
        if p.is_empty() {
            continue; // the root pair is already written
        }
        out.push_str(&format!("/{p}/\n!/{p}/*/\n"));
    }
    for d in &cone.recursive {
        if d.is_empty() {
            continue;
        }
        out.push_str(&format!("/{d}/\n"));
    }

    let path = pattern_path(repo);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("zvcs-tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(out.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

// --- config ----------------------------------------------------------------

fn worktree_config_path(repo: &gix::Repository) -> PathBuf {
    repo.git_dir().join("config.worktree")
}

/// Read a boolean from the worktree config, falling back to the repo-local one.
fn config_bool(repo: &gix::Repository, section: &str, key: &str) -> Result<Option<bool>> {
    for path in [worktree_config_path(repo), repo.common_dir().join("config")] {
        if !path.exists() {
            continue;
        }
        let file = ConfigFile::from_path_no_includes(path, Source::Local)?;
        if let Ok(v) = file.raw_value_by(section, None, key) {
            let v = v.to_str_lossy().to_ascii_lowercase();
            return Ok(Some(!matches!(v.as_str(), "false" | "no" | "off" | "0" | "")));
        }
    }
    Ok(None)
}

fn is_sparse(repo: &gix::Repository) -> Result<bool> {
    Ok(config_bool(repo, "core", "sparseCheckout")?.unwrap_or(false))
}

/// Cone mode is git's default whenever sparsity is on and nothing says otherwise.
fn is_cone(repo: &gix::Repository) -> Result<bool> {
    Ok(config_bool(repo, "core", "sparseCheckoutCone")?.unwrap_or(true))
}

/// Load (creating if absent) and mutate a config file, then persist atomically.
fn edit_config(path: &Path, edits: &[(&str, &str, &str)]) -> Result<()> {
    if !path.exists() {
        std::fs::write(path, b"")?;
    }
    let mut file = ConfigFile::from_path_no_includes(path.to_path_buf(), Source::Local)?;
    for (section, key, value) in edits {
        file.set_raw_value_by(*section, None, *key, *value)?;
    }
    let bytes = file.to_bstring();
    let tmp = path.with_extension("zvcs-tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Turn sparsity on exactly where git puts it: the per-worktree config, with
/// `extensions.worktreeConfig` opted in on the shared local config.
fn enable_config(repo: &gix::Repository) -> Result<()> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    edit_config(
        &repo.common_dir().join("config"),
        &[("extensions", "worktreeConfig", "true")],
    )?;
    edit_config(
        &worktree_config_path(repo),
        &[
            ("core", "sparseCheckout", "true"),
            ("core", "sparseCheckoutCone", "true"),
        ],
    )
}

fn disable_config(repo: &gix::Repository) -> Result<()> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    edit_config(
        &repo.common_dir().join("config"),
        &[("extensions", "worktreeConfig", "true")],
    )?;
    edit_config(
        &worktree_config_path(repo),
        &[
            ("core", "sparseCheckout", "false"),
            ("core", "sparseCheckoutCone", "false"),
            ("index", "sparse", "false"),
        ],
    )
}

// --- applying sparsity to index + worktree ---------------------------------

/// Reconcile the index `SKIP_WORKTREE` bits and the worktree files with `cone`
/// (`None` means "everything is included", i.e. `disable`).
fn apply(repo: &gix::Repository, cone: Option<&Cone>) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("this operation must be run in a work tree"))?
        .to_owned();

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let mut index = repo.open_index()?;

    // Snapshot identity per entry so the mutable pass below has no borrow on
    // the path backing.
    let snapshot: Vec<(BString, ObjectId, Mode)> = {
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .map(|e| (e.path_in(backing).to_owned(), e.id, e.mode))
            .collect()
    };

    let mut to_materialize: Vec<BString> = Vec::new();
    let mut to_remove: Vec<BString> = Vec::new();

    for (i, (path, id, mode)) in snapshot.iter().enumerate() {
        let included = match cone {
            None => true,
            Some(c) => c.matches(&path.to_str_lossy()),
        };
        let disk = repo.workdir_path(path.as_bstr());
        let exists = disk
            .as_ref()
            .map(|p| p.symlink_metadata().is_ok())
            .unwrap_or(false);

        let entry = &mut index.entries_mut()[i];
        if included {
            entry.flags.remove(Flags::SKIP_WORKTREE);
            if !entry.flags.contains(Flags::INTENT_TO_ADD) {
                entry.flags.remove(Flags::EXTENDED);
            }
            if !exists {
                to_materialize.push(path.clone());
            }
        } else {
            // git refuses to sparsify a path with local modifications: the file
            // stays, and so does its cleared skip bit.
            let dirty = exists
                && disk
                    .as_ref()
                    .map(|p| is_modified(repo, p, *id, *mode))
                    .unwrap_or(false);
            if dirty {
                entry.flags.remove(Flags::SKIP_WORKTREE);
                if !entry.flags.contains(Flags::INTENT_TO_ADD) {
                    entry.flags.remove(Flags::EXTENDED);
                }
            } else {
                // EXTENDED is what makes the skip bit survive serialization
                // (and forces index version 3, exactly as git does).
                entry.flags.insert(Flags::SKIP_WORKTREE | Flags::EXTENDED);
                if exists {
                    to_remove.push(path.clone());
                }
            }
        }
    }

    index.remove_tree();
    index.write(Default::default())?;

    if !to_materialize.is_empty() {
        // Re-open so the checkout sees the freshly cleared skip bits (entries
        // carrying SKIP_WORKTREE are ignored by the worktree writer).
        let mut subset = repo.open_index()?;
        subset.remove_entries(|_, path, _| !to_materialize.iter().any(|k| k.as_bstr() == path));
        checkout_subset(repo, &mut subset)?;
    }

    for path in &to_remove {
        let Some(full) = repo.workdir_path(path.as_bstr()) else {
            continue;
        };
        let _ = std::fs::remove_file(&full);
        prune_empty_dirs(&workdir, &full);
    }

    Ok(())
}

/// Whether the worktree file at `full` differs from the index blob `id`.
fn is_modified(repo: &gix::Repository, full: &Path, id: ObjectId, mode: Mode) -> bool {
    let content = if mode.to_tree_entry_mode().is_some_and(|m| m.is_link()) {
        match std::fs::read_link(full) {
            Ok(t) => gix::path::into_bstr(t).into_owned().into(),
            Err(_) => return false,
        }
    } else {
        match std::fs::read(full) {
            Ok(c) => c,
            Err(_) => return false,
        }
    };
    match gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, &content) {
        Ok(actual) => actual != id,
        // If we cannot hash it we must not delete it.
        Err(_) => true,
    }
}

/// Remove the now-empty ancestor directories of `full`, stopping at `workdir`
/// or at the first directory that still holds something.
fn prune_empty_dirs(workdir: &Path, full: &Path) {
    let mut cur = full.parent();
    while let Some(dir) = cur {
        if dir == workdir || !dir.starts_with(workdir) {
            break;
        }
        if std::fs::remove_dir(dir).is_err() {
            break;
        }
        cur = dir.parent();
    }
}

/// Write every entry of `index` into the worktree (same helper shape the other
/// worktree-mutating porcelain uses).
fn checkout_subset(repo: &gix::Repository, index: &mut gix::index::File) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("bare repository has no worktree to update"))?
        .to_owned();
    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    let should_interrupt = AtomicBool::new(false);
    gix::worktree::state::checkout(
        index,
        workdir.as_path(),
        odb,
        &gix::progress::Discard,
        &gix::progress::Discard,
        &should_interrupt,
        opts,
    )?;
    Ok(())
}

// --- path quoting ----------------------------------------------------------

/// Undo git's C-style quoting when `input` is a quoted path; otherwise return
/// it unchanged.
fn unquote_c(input: &[u8]) -> Result<BString> {
    if !input.starts_with(b"\"") {
        return Ok(BString::from(input));
    }
    let body = input
        .strip_prefix(b"\"")
        .and_then(|r| r.strip_suffix(b"\""))
        .ok_or_else(|| anyhow!("unterminated quoted path"))?;

    let mut out: Vec<u8> = Vec::with_capacity(body.len());
    let mut it = body.iter().copied();
    while let Some(b) = it.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        let e = it.next().ok_or_else(|| anyhow!("trailing backslash in quoted path"))?;
        match e {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b't' => out.push(b'\t'),
            b'n' => out.push(b'\n'),
            b'v' => out.push(0x0b),
            b'f' => out.push(0x0c),
            b'r' => out.push(b'\r'),
            b'"' => out.push(b'"'),
            b'\\' => out.push(b'\\'),
            d if d.is_ascii_digit() => {
                // Three-digit octal escape, the first digit already consumed.
                let d2 = it.next().ok_or_else(|| anyhow!("truncated octal escape"))?;
                let d3 = it.next().ok_or_else(|| anyhow!("truncated octal escape"))?;
                let val = u32::from(d - b'0') * 64 + u32::from(d2 - b'0') * 8 + u32::from(d3 - b'0');
                out.push(u8::try_from(val).map_err(|_| anyhow!("octal escape out of range"))?);
            }
            other => bail!("unknown escape \\{} in quoted path", other as char),
        }
    }
    Ok(BString::from(out))
}

/// C-style path quoting matching git's default `core.quotePath=true`: a path is
/// wrapped in double quotes and escaped when it contains control bytes, a quote,
/// a backslash, or any byte >= 0x80; otherwise it is emitted verbatim.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    let bytes = path.as_ref();
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    /// The generated file must match stock git's cone layout exactly: root pair,
    /// then sorted parent pairs, then sorted recursive lines.
    #[test]
    fn cone_file_layout_round_trips() {
        let cone = Cone::new(set(&["a/b/c", "q", "z/y"]));
        let mut rendered = String::from("/*\n!/*/\n");
        for p in cone.parents.iter().filter(|p| !p.is_empty()) {
            rendered.push_str(&format!("/{p}/\n!/{p}/*/\n"));
        }
        for d in &cone.recursive {
            rendered.push_str(&format!("/{d}/\n"));
        }
        assert_eq!(
            rendered,
            "/*\n!/*/\n/a/\n!/a/*/\n/a/b/\n!/a/b/*/\n/z/\n!/z/*/\n/a/b/c/\n/q/\n/z/y/\n"
        );

        let lines: Vec<String> = rendered.lines().map(str::to_owned).collect();
        assert_eq!(cone_dirs(&lines), set(&["a/b/c", "q", "z/y"]));
    }

    /// Cone membership: files directly in an ancestor are in, files at any depth
    /// under a recursive directory are in, everything else is out.
    #[test]
    fn cone_membership() {
        let cone = Cone::new(set(&["a/b"]));
        for inside in ["top", "a/5", "a/b/4", "a/b/c/3"] {
            assert!(cone.matches(inside), "{inside} should be inside");
        }
        for outside in ["q/6", "z/2", "z/y/1", "ab/1"] {
            assert!(!cone.matches(outside), "{outside} should be outside");
        }
    }

    /// A directory already covered by a recursive ancestor is dropped, matching
    /// git collapsing `set a a/b` down to `/a/`.
    #[test]
    fn nested_directories_collapse() {
        assert_eq!(dedup_nested(set(&["a", "a/b", "ab"])), set(&["a", "ab"]));
    }

    #[test]
    fn leading_slash_is_rejected_as_a_pattern() {
        assert_eq!(normalize_dir("/a").unwrap(), None);
        assert_eq!(normalize_dir("a/").unwrap(), Some("a".to_owned()));
        assert_eq!(normalize_dir("\"w x\"").unwrap(), Some("w x".to_owned()));
    }

    #[test]
    fn quoted_paths_round_trip() {
        assert_eq!(unquote_c(b"\"a\\tb\"").unwrap(), BString::from("a\tb"));
        assert_eq!(unquote_c(b"w x").unwrap(), BString::from("w x"));
        assert_eq!(quote_path(b"w x"), "w x");
        assert_eq!(quote_path(b"a\tb"), "\"a\\tb\"");
    }
}
