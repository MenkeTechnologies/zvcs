//! `git rm [<options>] <pathspec>...` — remove tracked paths from the index and
//! (unless `--cached`) from the working tree.
//!
//! Served natively through the vendored gitoxide crates so tools on PATH observe
//! the same staged index. The full stock `git rm` flag surface is reproduced:
//!
//!   * `--cached`                     — remove from the index only, keep the file
//!   * `-f`, `--force`                — skip the up-to-date safety check
//!   * `-r`                           — allow recursive removal of a directory pathspec
//!   * `-n`, `--dry-run`              — report what would be removed, change nothing
//!   * `--ignore-unmatch`             — exit 0 even if a pathspec matched nothing
//!   * `-q`, `--quiet`                — suppress the `rm '<path>'` lines
//!   * `--sparse`                     — accepted (sparse-checkout cone; see note below)
//!   * `--pathspec-from-file=<file>`  — read pathspecs from `<file>` (or stdin with `-`)
//!   * `--pathspec-file-nul`          — NUL-separated pathspec file entries
//!   * `--`, `--end-of-options`       — end option parsing
//!
//! Long options accept unambiguous abbreviations (`--dry`, `--cach`) and `--no-`
//! negations (`--no-cached`), matching git's parse-options; the last spelling of a
//! toggle wins. Unknown options/switches exit 129 with git's usage block; an empty
//! or missing pathspec exits 128 ("No pathspec was given").
//!
//! Faithfully reproduced: literal, glob, and full magic-signature pathspecs
//! (`:(glob)`, `:(literal)`, `:(icase)`, `:(top)`, `:(exclude)`/`:!`, `:(attr:…)`)
//! via the shared `repo.pathspec()` engine; the per-spec matched/recursion rules
//! (`did not match any files`, `not removing '<x>' recursively without -r`); the
//! index-vs-HEAD and worktree-vs-index safety check (raw blob hashing; conservative
//! — a filtered worktree that differs at the byte level is reported as modified, so
//! `-f` is required, never silently discarded); submodule (gitlink) removal
//! including recursive worktree pruning and `.gitmodules` section removal + staging;
//! and the `rm '<path>'` output in index order.
//!
//! Unmerged (conflicted) paths are removable without `-f` (all stages dropped),
//! exactly as stock git does.
//!
//! Deviations kept honest: `--sparse` is accepted but the sparse-checkout *cone*
//! exclusion it guards is not enforced here (a no-op outside a sparse checkout,
//! which is the only place the flag changes behavior); pathspec files are not
//! C-style unquoted (git-generated pathspec files are not quoted).

use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::Mode;
use gix::pathspec::search::MatchKind;

/// git's `git rm` usage block, emitted verbatim on a usage error (exit 129).
const USAGE: &str = "\
usage: git rm [-f | --force] [-n] [-r] [--cached] [--ignore-unmatch]
              [--quiet] [--pathspec-from-file=<file> [--pathspec-file-nul]]
              [--] [<pathspec>...]
";

/// A tracked path selected for removal, captured before the index is mutated.
struct Target {
    path: BString,
    id: ObjectId,
    mode: Mode,
    stage: u32,
}

/// Parsed option state.
#[derive(Default)]
struct Opts {
    cached: bool,
    force: bool,
    recursive: bool,
    dry_run: bool,
    ignore_unmatch: bool,
    quiet: bool,
    #[allow(dead_code)]
    sparse: bool,
    pathspec_from_file: Option<String>,
    pathspec_file_nul: bool,
}

/// The long options `git rm` accepts, in the order git registers them.
#[derive(Clone, Copy, PartialEq)]
enum Long {
    DryRun,
    Quiet,
    Cached,
    Force,
    IgnoreUnmatch,
    Sparse,
    PathspecFromFile,
    PathspecFileNul,
}

const LONGS: &[(&str, Long)] = &[
    ("dry-run", Long::DryRun),
    ("quiet", Long::Quiet),
    ("cached", Long::Cached),
    ("force", Long::Force),
    ("ignore-unmatch", Long::IgnoreUnmatch),
    ("sparse", Long::Sparse),
    ("pathspec-from-file", Long::PathspecFromFile),
    ("pathspec-file-nul", Long::PathspecFileNul),
];

/// `error: <msg>` + usage, exit 129 (git's usage-error convention).
fn usage_err(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {msg}");
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// `fatal: <msg>`, exit 128 (git's fatal convention).
fn fatal(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// Resolve a long-option name (already stripped of `--` and any `no-`/`=value`) to
/// its canonical option, honoring exact-match precedence then unambiguous prefix.
fn resolve_long(name: &str) -> std::result::Result<Long, ExitCode> {
    if let Some((_, opt)) = LONGS.iter().find(|(n, _)| *n == name) {
        return Ok(*opt);
    }
    let hits: Vec<Long> = LONGS
        .iter()
        .filter(|(n, _)| n.starts_with(name))
        .map(|(_, o)| *o)
        .collect();
    match hits.len() {
        1 => Ok(hits[0]),
        0 => Err(usage_err(format!("unknown option `{name}'"))),
        _ => Err(usage_err(format!("ambiguous option: {name}"))),
    }
}

pub fn rm(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts::default();
    let mut pathspecs: Vec<String> = Vec::new();
    let mut opts_done = false;

    // 1. Parse flags. Mirrors git's parse-options: `--` / `--end-of-options`
    //    terminate; long options abbreviate and take `--no-` negations; short
    //    flags cluster. Toggles are last-wins.
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if opts_done {
            pathspecs.push(a.clone());
            i += 1;
            continue;
        }
        if a == "--" || a == "--end-of-options" {
            opts_done = true;
            i += 1;
            continue;
        }
        if let Some(body) = a.strip_prefix("--") {
            let (name, inline_val) = match body.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (body, None),
            };
            let (negate, bare) = match name.strip_prefix("no-") {
                Some(rest) => (true, rest),
                None => (false, name),
            };
            let opt = match resolve_long(bare) {
                Ok(o) => o,
                Err(code) => return Ok(code),
            };
            // Value option.
            if opt == Long::PathspecFromFile {
                if negate {
                    opts.pathspec_from_file = None;
                    i += 1;
                    continue;
                }
                let val = match inline_val {
                    Some(v) => v,
                    None => match args.get(i + 1) {
                        Some(v) => {
                            i += 1;
                            v.clone()
                        }
                        None => return Ok(usage_err(format!("option `{bare}' requires a value"))),
                    },
                };
                opts.pathspec_from_file = Some(val);
                i += 1;
                continue;
            }
            // Boolean options never take an inline value.
            if inline_val.is_some() {
                return Ok(usage_err(format!("option `{bare}' takes no value")));
            }
            let on = !negate;
            match opt {
                Long::DryRun => opts.dry_run = on,
                Long::Quiet => opts.quiet = on,
                Long::Cached => opts.cached = on,
                Long::Force => opts.force = on,
                Long::IgnoreUnmatch => opts.ignore_unmatch = on,
                Long::Sparse => opts.sparse = on,
                Long::PathspecFileNul => opts.pathspec_file_nul = on,
                Long::PathspecFromFile => unreachable!(),
            }
            i += 1;
            continue;
        }
        if a.len() > 1 && a.starts_with('-') {
            for c in a[1..].chars() {
                match c {
                    'f' => opts.force = true,
                    'r' => opts.recursive = true,
                    'n' => opts.dry_run = true,
                    'q' => opts.quiet = true,
                    _ => return Ok(usage_err(format!("unknown switch `{c}'"))),
                }
            }
            i += 1;
            continue;
        }
        // A bare `-` or any non-option token is a pathspec.
        pathspecs.push(a.clone());
        i += 1;
    }

    // 2. --pathspec-from-file: mutually exclusive with cmdline pathspecs, read
    //    before the empty-pathspec check (both fatal, exit 128).
    if let Some(file) = &opts.pathspec_from_file {
        if !pathspecs.is_empty() {
            return Ok(fatal(
                "'--pathspec-from-file' and pathspec arguments cannot be used together",
            ));
        }
        match read_pathspec_file(file, opts.pathspec_file_nul) {
            Ok(v) => pathspecs = v,
            Err(code) => return Ok(code),
        }
    }

    if pathspecs.is_empty() {
        return Ok(fatal("No pathspec was given. Which files should I remove?"));
    }

    // 3. Open the repository and require a working tree.
    let repo = gix::discover(".")?;
    let workdir = match repo.workdir() {
        Some(w) => w.to_owned(),
        None => return Ok(fatal("this operation must be run in a work tree")),
    };

    // Serialize the whole read-modify-write of the index through the repo
    // coordinator so concurrent zvcs writers queue FCFS instead of racing
    // `index.lock`. Held for the rest of the function; a no-op with no daemon.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // 4. Validate every pathspec up front: bad magic (`:(bogus)…`) or a spec that
    //    escapes the worktree is fatal (exit 128), exactly like git — before any
    //    matching or mutation. Also records which specs are exclusions, since
    //    git's "did not match" report skips exclude specs.
    let defaults = repo.pathspec_defaults_inherit_ignore_case(false)?;
    let prefix = repo.prefix()?.map(|p| p.to_path_buf()).unwrap_or_default();
    let root = gix::path::realpath(repo.workdir().unwrap_or_else(|| repo.git_dir()))?;
    let mut is_exclude: Vec<bool> = Vec::with_capacity(pathspecs.len());
    let mut patterns: Vec<BString> = Vec::with_capacity(pathspecs.len());
    for raw in &pathspecs {
        let mut parsed = match gix::pathspec::parse(raw.as_bytes(), defaults) {
            Ok(p) => p,
            Err(_) => return Ok(fatal(format!("{raw}: bad pathspec magic"))),
        };
        is_exclude.push(parsed.is_excluded());
        if parsed.normalize(&prefix, &root).is_err() {
            return Ok(fatal(format!(
                "{raw}: '{}' is outside repository at '{}'",
                parsed.path().to_str_lossy(),
                root.display()
            )));
        }
        patterns.push(BString::from(raw.as_str()));
    }

    // 5. Snapshot the index entries (owned) so matching/safety reads don't hold a
    //    borrow across the later mutation.
    let index = repo.open_index()?;
    let targets_all: Vec<Target> = {
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .map(|e| Target {
                path: e.path_in(backing).to_owned(),
                id: e.id,
                mode: e.mode,
                stage: e.stage_raw(),
            })
            .collect()
    };

    // 6. Match pathspecs against the index via the shared pathspec engine. Track
    //    per-spec how each matched, mirroring git's `seen[]`: RECURSIVELY (a
    //    directory prefix), FNMATCH (wildcard), or EXACTLY (verbatim). A spec that
    //    only ever matched RECURSIVELY needs `-r`.
    const RECURSIVELY: u8 = 1;
    const FNMATCH: u8 = 3;
    const EXACTLY: u8 = 4;

    let mut ps = repo.pathspec(
        true,
        &patterns,
        false,
        &index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;

    let mut seen: Vec<u8> = vec![0; pathspecs.len()];
    // The synthetic "matched because nothing excludes it" case (all specs are
    // exclusions) reports sequence_number == pathspecs.len(); track it apart.
    let mut synthetic_seen: u8 = 0;
    let mut selected: Vec<Target> = Vec::new();
    let mut selected_paths: HashSet<BString> = HashSet::new();

    for t in &targets_all {
        let Some(m) = ps.pattern_matching_relative_path(t.path.as_bstr(), Some(false)) else {
            continue;
        };
        if m.is_excluded() {
            continue;
        }
        let rank = match m.kind {
            MatchKind::Prefix => RECURSIVELY,
            MatchKind::WildcardMatch => FNMATCH,
            MatchKind::Verbatim => EXACTLY,
            // A whole-tree match (empty/synthetic pattern) is a recursive match.
            MatchKind::Always => RECURSIVELY,
        };
        if m.sequence_number < seen.len() {
            if seen[m.sequence_number] < rank {
                seen[m.sequence_number] = rank;
            }
        } else if synthetic_seen < rank {
            synthetic_seen = rank;
        }
        if selected_paths.insert(t.path.clone()) {
            selected.push(Target {
                path: t.path.clone(),
                id: t.id,
                mode: t.mode,
                stage: t.stage,
            });
        }
    }

    // 7. Per-spec validation loop, in argument order, exactly like git: excludes
    //    are skipped; an unmatched positive spec is fatal unless --ignore-unmatch;
    //    a spec that matched only recursively is fatal without -r.
    for (idx, raw) in pathspecs.iter().enumerate() {
        if is_exclude[idx] {
            continue;
        }
        let how = seen[idx];
        if how == 0 {
            if opts.ignore_unmatch {
                continue;
            }
            return Ok(fatal(format!("pathspec '{raw}' did not match any files")));
        }
        if !opts.recursive && how == RECURSIVELY {
            return Ok(fatal(format!("not removing '{raw}' recursively without -r")));
        }
    }
    // The all-exclusions case: git treats the implicit whole-tree match as `.`.
    if is_exclude.iter().all(|&e| e) && synthetic_seen == RECURSIVELY && !opts.recursive {
        return Ok(fatal("not removing '.' recursively without -r"));
    }

    if selected.is_empty() {
        // Only reachable when every unmatched spec was ignored.
        return Ok(ExitCode::SUCCESS);
    }

    // 8. Submodule removals need `.gitmodules` in a clean staging state, and the
    //    section for each removed submodule stripped and restaged. Resolve the
    //    path→name map (from the current config) before any mutation.
    let has_submodule = selected.iter().any(|t| t.mode == Mode::COMMIT);
    let mut submodule_name_by_path: HashMap<BString, BString> = HashMap::new();
    if has_submodule {
        if let Some(modules) = repo.submodules()? {
            for sm in modules {
                if let Ok(p) = sm.path() {
                    submodule_name_by_path.insert(p, sm.name().to_owned());
                }
            }
        }
        // git refuses to proceed with unstaged `.gitmodules` edits.
        if gitmodules_has_unstaged_changes(&repo, &index)? {
            return Ok(fatal(
                "please stage your changes to .gitmodules or stash them to proceed",
            ));
        }
    }

    // 9. Up-to-date safety check (skipped with -f). Unmerged (stage != 0) paths are
    //    always removable and bypass it. Per stage-0 path:
    //      staged = index blob differs from HEAD blob
    //      local  = worktree content differs from index blob (missing == no change);
    //               for a submodule, "local" means the submodule worktree is dirty.
    //    Full removal refuses on staged OR local; --cached refuses only when the
    //    staged content matches neither HEAD nor the worktree (staged AND local).
    if !opts.force {
        let hash_kind = repo.object_hash();
        let head_tree = repo.head_tree().ok();

        let mut both: Vec<String> = Vec::new();
        let mut staged_only: Vec<String> = Vec::new();
        let mut local_only: Vec<String> = Vec::new();

        for t in &selected {
            if t.stage != 0 {
                continue; // unmerged: always removable
            }
            let path_str = t.path.to_str_lossy().into_owned();

            let head_id: Option<ObjectId> = match &head_tree {
                Some(tree) => tree
                    .lookup_entry_by_path(std::path::Path::new(&path_str))?
                    .map(|e| e.id().detach()),
                None => None,
            };
            let staged = head_id.map(|h| h != t.id).unwrap_or(true);

            let local = if t.mode == Mode::COMMIT {
                submodule_is_dirty(&repo, &t.path)
            } else {
                match worktree_blob(&repo, &t.path, t.mode, hash_kind)? {
                    Some(wt_id) => wt_id != t.id,
                    None => false, // already gone from the worktree
                }
            };

            match (staged, local) {
                (true, true) => both.push(path_str),
                (true, false) => staged_only.push(path_str),
                (false, true) => local_only.push(path_str),
                (false, false) => {}
            }
        }

        // Assemble the refusal exactly along git's categories (exit 1).
        let mut blocks: Vec<String> = Vec::new();
        let plural = |v: &[String]| if v.len() == 1 { ("file", "has") } else { ("files", "have") };
        if !both.is_empty() {
            let (f, h) = plural(&both);
            blocks.push(format!(
                "the following {f} {h} staged content different from both the file and the HEAD:\n    {}",
                both.join("\n    ")
            ));
        }
        if !opts.cached && !staged_only.is_empty() {
            let (f, h) = plural(&staged_only);
            blocks.push(format!(
                "the following {f} {h} changes staged in the index:\n    {}",
                staged_only.join("\n    ")
            ));
        }
        if !opts.cached && !local_only.is_empty() {
            let (f, h) = plural(&local_only);
            blocks.push(format!(
                "the following {f} {h} local modifications:\n    {}",
                local_only.join("\n    ")
            ));
        }
        if !blocks.is_empty() {
            let joined = blocks.join("\nerror: ");
            if crate::advice::enabled("rmHints") {
                let hint = if opts.cached {
                    "(use -f to force removal)"
                } else {
                    "(use --cached to keep the file, or -f to force removal)"
                };
                eprintln!("error: {joined}\n{hint}");
            } else {
                eprintln!("error: {joined}");
            }
            return Ok(ExitCode::from(1));
        }
    }

    // 10. Print the removals (index order) unless quiet. Done before mutating so
    //     dry-run and real runs report identically. Paths are emitted as raw bytes
    //     in single quotes (git applies no quoting to `rm '%s'`).
    if !opts.quiet {
        let mut out = Vec::new();
        for t in &selected {
            out.extend_from_slice(b"rm '");
            out.extend_from_slice(t.path.as_bytes());
            out.extend_from_slice(b"'\n");
        }
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        lock.write_all(&out)?;
        lock.flush()?;
    }

    if opts.dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    // 11. Remove the selected worktree files first (unless --cached), pruning any
    //     leading directories left empty. Submodule (gitlink) paths are directories
    //     and are removed recursively (their gitdir under .git/modules survives).
    if !opts.cached {
        for t in &selected {
            let Some(abs) = repo.workdir_path(t.path.as_bstr()) else {
                continue;
            };
            let res = if t.mode == Mode::COMMIT {
                std::fs::remove_dir_all(&abs)
            } else {
                std::fs::remove_file(&abs)
            };
            match res {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => bail!("failed to remove {}: {e}", t.path.to_str_lossy()),
            }
            // Prune now-empty parent directories up to (never including) workdir.
            let mut cur = abs.parent().map(|p| p.to_owned());
            while let Some(dir) = cur {
                if dir == workdir || std::fs::remove_dir(&dir).is_err() {
                    break;
                }
                cur = dir.parent().map(|p| p.to_owned());
            }
        }
    }

    // 12. For removed submodules, strip their `.gitmodules` sections and restage
    //     the edited file. Only for full removal — `--cached` leaves `.gitmodules`
    //     untouched (the worktree submodule survives, now untracked), matching git.
    let mut index = index;
    let gitmodules_update = if has_submodule && !opts.cached {
        let removed_paths: Vec<&BString> = selected
            .iter()
            .filter(|t| t.mode == Mode::COMMIT)
            .map(|t| &t.path)
            .collect();
        update_gitmodules(&repo, &workdir, &removed_paths, &submodule_name_by_path)?
    } else {
        None
    };

    // 13. Drop every selected path (all stages) from the owned index, apply any
    //     `.gitmodules` restage, and persist.
    index.remove_entries(|_, path, _| selected_paths.contains(&path.to_owned()));
    if let Some((id, stat)) = gitmodules_update {
        index.remove_entries(|_, path, _| path == b".gitmodules".as_bstr());
        index.dangerously_push_entry(
            stat,
            id,
            gix::index::entry::Flags::empty(),
            Mode::FILE,
            b".gitmodules".as_bstr(),
        );
        index.sort_entries();
    }
    // The cache-tree extension is written as-is, so drop it after mutating
    // entries or a later commit could capture a stale subtree.
    index.remove_tree();
    index.write(gix::index::write::Options::default())?;

    Ok(ExitCode::SUCCESS)
}

/// Read pathspecs from `spec` (a file path, or `-` for stdin). Entries are split
/// on NUL when `nul`, else on newline with a trailing `\r` stripped; empty entries
/// are dropped. A missing/unreadable file is fatal (exit 128) like git.
fn read_pathspec_file(spec: &str, nul: bool) -> std::result::Result<Vec<String>, ExitCode> {
    let data = if spec == "-" {
        let mut b = Vec::new();
        if std::io::stdin().read_to_end(&mut b).is_err() {
            return Err(fatal("could not read pathspec from stdin"));
        }
        b
    } else {
        match std::fs::read(spec) {
            Ok(b) => b,
            Err(e) => return Err(fatal(format!("could not open '{spec}' for reading: {e}"))),
        }
    };

    let mut out = Vec::new();
    let sep = if nul { 0u8 } else { b'\n' };
    for part in data.split(|&b| b == sep) {
        let mut p = part;
        if !nul && p.last() == Some(&b'\r') {
            p = &p[..p.len() - 1];
        }
        if p.is_empty() {
            continue;
        }
        out.push(String::from_utf8_lossy(p).into_owned());
    }
    Ok(out)
}

/// Hash the working-tree content at `path` into its git blob id, or `None` if the
/// file is absent. Symlinks hash their target string (as git stores them); an
/// unreadable file is treated as changed (conservative — forces `-f`).
fn worktree_blob(
    repo: &gix::Repository,
    path: &BString,
    mode: Mode,
    hash_kind: gix::hash::Kind,
) -> Result<Option<ObjectId>> {
    let Some(abs) = repo.workdir_path(path.as_bstr()) else {
        return Ok(None);
    };
    let meta = match std::fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => bail!("failed to stat {}: {e}", path.to_str_lossy()),
    };

    let content: Vec<u8> = if mode == Mode::SYMLINK || meta.is_symlink() {
        use std::os::unix::ffi::OsStrExt;
        std::fs::read_link(&abs)
            .map_err(|e| anyhow::anyhow!("failed to read symlink {}: {e}", path.to_str_lossy()))?
            .as_os_str()
            .as_bytes()
            .to_vec()
    } else {
        std::fs::read(&abs)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.to_str_lossy()))?
    };

    let id = gix::objs::compute_hash(hash_kind, gix::objs::Kind::Blob, &content)?;
    Ok(Some(id))
}

/// Whether the submodule rooted at `path` has changes that make it "modified" for
/// `git rm`'s safety check (worktree modifications, untracked files, or a checked
/// out HEAD that differs from the recorded gitlink). Missing/unopenable submodules
/// are treated as clean (nothing to lose).
fn submodule_is_dirty(repo: &gix::Repository, path: &BString) -> bool {
    let Ok(Some(modules)) = repo.submodules() else {
        return false;
    };
    for sm in modules {
        if sm.path().map(|p| &p == path).unwrap_or(false) {
            return match sm.status(gix::submodule::config::Ignore::None, true) {
                Ok(status) => status.is_dirty().unwrap_or(false),
                Err(_) => false,
            };
        }
    }
    false
}

/// True when the worktree `.gitmodules` differs from its staged (index) blob — the
/// condition under which git refuses to touch `.gitmodules` during `rm`.
fn gitmodules_has_unstaged_changes(
    repo: &gix::Repository,
    index: &gix::index::State,
) -> Result<bool> {
    let name = b".gitmodules".as_bstr();
    let Some(entry) = index.entry_by_path(name) else {
        return Ok(false); // not tracked → nothing to conflict with
    };
    let Some(abs) = repo.workdir_path(name) else {
        return Ok(false);
    };
    let content = match std::fs::read(&abs) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => bail!("failed to read .gitmodules: {e}"),
    };
    let id = gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, &content)?;
    Ok(id != entry.id)
}

/// Strip the `[submodule "<name>"]` sections of every removed submodule from the
/// worktree `.gitmodules`, write it back, and return the staged blob id + stat.
/// Returns `None` when `.gitmodules` is absent or unchanged.
fn update_gitmodules(
    repo: &gix::Repository,
    workdir: &std::path::Path,
    removed_paths: &[&BString],
    name_by_path: &HashMap<BString, BString>,
) -> Result<Option<(ObjectId, gix::index::entry::Stat)>> {
    let gm_path = workdir.join(".gitmodules");
    let mut content = match std::fs::read(&gm_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => bail!("failed to read .gitmodules: {e}"),
    };

    let mut changed = false;
    for path in removed_paths {
        if let Some(name) = name_by_path.get(*path) {
            if remove_gitmodules_section(&mut content, name.as_bytes()) {
                changed = true;
            }
        }
    }
    if !changed {
        return Ok(None);
    }

    std::fs::write(&gm_path, &content)?;
    let id = repo.write_blob(&content)?.detach();
    let md = gix::index::fs::Metadata::from_path_no_follow(&gm_path)?;
    let stat = gix::index::entry::Stat::from_fs(&md).unwrap_or_default();
    Ok(Some((id, stat)))
}

/// Delete the `[submodule "<name>"]` section from git-config bytes, spanning from
/// the header line to the next section header (a line beginning with `[`) or EOF —
/// matching git's byte-range section removal for a git-generated `.gitmodules`.
/// Returns whether a section was removed.
fn remove_gitmodules_section(content: &mut Vec<u8>, name: &[u8]) -> bool {
    let mut header = Vec::with_capacity(name.len() + 16);
    header.extend_from_slice(b"[submodule \"");
    header.extend_from_slice(name);
    header.extend_from_slice(b"\"]");

    // Find the header line (its content, ignoring leading/trailing ASCII space).
    let mut line_start = 0usize;
    let mut section_start = None;
    while line_start <= content.len() {
        let line_end = content[line_start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| line_start + p)
            .unwrap_or(content.len());
        let line = &content[line_start..line_end];
        let trimmed = trim_ascii(line);
        if trimmed == header.as_slice() {
            section_start = Some(line_start);
            break;
        }
        if line_end == content.len() {
            break;
        }
        line_start = line_end + 1;
    }
    let Some(start) = section_start else {
        return false;
    };

    // Find the next section header line at or after the following line.
    let mut cursor = content[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|p| start + p + 1)
        .unwrap_or(content.len());
    let mut end = content.len();
    while cursor < content.len() {
        let line_end = content[cursor..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| cursor + p)
            .unwrap_or(content.len());
        let trimmed = trim_ascii(&content[cursor..line_end]);
        if trimmed.first() == Some(&b'[') {
            end = cursor;
            break;
        }
        if line_end == content.len() {
            break;
        }
        cursor = line_end + 1;
    }

    content.drain(start..end);
    true
}

/// Trim leading/trailing ASCII whitespace from a byte slice.
fn trim_ascii(mut b: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = b {
        if first.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = b {
        if last.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    b
}
