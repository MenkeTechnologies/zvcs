//! `git stage` — stage worktree paths into the index, served natively via the
//! vendored gitoxide crates so tools on PATH see the same staged index.
//!
//! Stock git implements `stage` as an alias entry in its command table pointing
//! at the very same `cmd_add` C function (`git-stage(1)`: "This is a synonym for
//! git-add(1)"), so the semantics below are `git add`'s semantics.
//!
//! Supported forms:
//!   * `git stage <pathspec>...`   — stage files/dirs (recurses, honors `.gitignore`)
//!   * `-A`/`--all`/`--no-ignore-removal`   — adds, modifications *and* deletions
//!   * `--no-all`/`--ignore-removal`        — adds and modifications, no deletions
//!   * `-u`/`--update` / `--no-update`      — restage tracked paths only
//!   * `-N`/`--intent-to-add`               — record untracked paths as empty blobs
//!   * `--refresh`                          — refresh stat info only, stage nothing
//!   * `--chmod=+x|-x`                      — force the index mode of matched paths
//!   * `--ignore-errors`                    — skip unreadable files, exit 1
//!   * `--ignore-missing` (with `--dry-run`) — non-matching pathspecs are not fatal
//!   * `-n/--dry-run`, `-v/--verbose`, `-f/--force`, `--sparse/--no-sparse`, `--`
//!
//! Deviations (bailed or noted, never faked):
//!   * `.gitattributes` content filters (autocrlf, `clean`/`smudge`) are NOT
//!     applied — the blob is the verbatim worktree bytes. `--renormalize` exists
//!     only to re-run those filters, so it is rejected outright whenever the repo
//!     is configured in a way that could engage one.
//!   * `--sparse`/`--no-sparse` are accepted only while the repo has no
//!     sparse-checkout; with one configured they are rejected rather than ignored.
//!   * submodule gitlinks are skipped here (use `git zbump`).
//!   * interactive/patch/edit and `--pathspec-from-file` are rejected with a
//!     precise message rather than silently ignored.
//!   * pathspecs are resolved relative to the repository root, not to the current
//!     working directory's prefix.
//!
//! NOTE: this module currently duplicates the staging engine that [`add`](super::add)
//! also carries. The two should be hoisted into one shared engine that both verbs
//! call; this copy is the git-accurate one.

use anyhow::{bail, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::index::entry::{Flags, Mode, Stage, Stat};

/// Exit code git uses for a fatal error.
const FATAL: u8 = 128;

// ---------------------------------------------------------------------------
// options
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Opts {
    dry_run: bool,
    verbose: bool,
    force: bool,
    /// `-A`/`--all` is `Some(true)`, `--no-all`/`--ignore-removal` is `Some(false)`.
    /// `None` is git's default, which stages deletions for the paths it matched.
    addremove: Option<bool>,
    update: bool,
    intent_to_add: bool,
    refresh: bool,
    renormalize: bool,
    ignore_errors: bool,
    ignore_missing: bool,
    sparse: Option<bool>,
    /// `Some(true)` for `--chmod=+x`, `Some(false)` for `--chmod=-x`.
    chmod: Option<bool>,
    pathspec_file_nul: bool,
    pathspec_from_file: bool,
    pathspecs: Vec<String>,
}

impl Opts {
    /// Deletions are staged unless `--no-all`/`--ignore-removal` turned them off.
    /// `-u` restages tracked paths, which includes removing the vanished ones.
    fn stages_deletions(&self) -> bool {
        self.addremove.unwrap_or(true)
    }
}

/// Parse the argument vector the way git's `parse_options` does for `cmd_add`:
/// every toggle honours its `--no-` twin and the last occurrence wins.
fn parse(args: &[String]) -> Result<Opts> {
    let mut o = Opts::default();
    let mut positional_only = false;

    for a in args {
        if positional_only {
            o.pathspecs.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => positional_only = true,

            "-n" | "--dry-run" => o.dry_run = true,
            "--no-dry-run" => o.dry_run = false,
            "-v" | "--verbose" => o.verbose = true,
            "--no-verbose" => o.verbose = false,
            "-f" | "--force" => o.force = true,
            "--no-force" => o.force = false,

            // `--all` and `--no-ignore-removal` are the same switch, as are
            // `--no-all` and `--ignore-removal` (git-add(1), "--no-all").
            "-A" | "--all" | "--no-ignore-removal" => o.addremove = Some(true),
            "--no-all" | "--ignore-removal" => o.addremove = Some(false),

            "-u" | "--update" => o.update = true,
            "--no-update" => o.update = false,
            "-N" | "--intent-to-add" => o.intent_to_add = true,
            "--no-intent-to-add" => o.intent_to_add = false,
            "--refresh" => o.refresh = true,
            "--no-refresh" => o.refresh = false,
            "--renormalize" => o.renormalize = true,
            "--no-renormalize" => o.renormalize = false,
            "--ignore-errors" => o.ignore_errors = true,
            "--no-ignore-errors" => o.ignore_errors = false,
            "--ignore-missing" => o.ignore_missing = true,
            "--no-ignore-missing" => o.ignore_missing = false,
            "--sparse" => o.sparse = Some(true),
            "--no-sparse" => o.sparse = Some(false),
            "--pathspec-file-nul" => o.pathspec_file_nul = true,
            "--no-pathspec-file-nul" => o.pathspec_file_nul = false,

            "--chmod=+x" => o.chmod = Some(true),
            "--chmod=-x" => o.chmod = Some(false),
            other if other.starts_with("--chmod=") => {
                // git: `fatal: --chmod param '<v>' must be either -x or +x`
                let value = &other["--chmod=".len()..];
                eprintln!("fatal: --chmod param '{value}' must be either -x or +x");
                std::process::exit(i32::from(FATAL));
            }

            other if other == "--pathspec-from-file" || other.starts_with("--pathspec-from-file=") => {
                o.pathspec_from_file = true;
            }

            // Recognized git flags that this port does not implement: name them.
            "-p" | "--patch" => bail!("interactive patch mode (-p/--patch) is not supported"),
            "-i" | "--interactive" => bail!("interactive mode (-i/--interactive) is not supported"),
            "-e" | "--edit" => bail!("--edit is not supported"),

            // Bundled short flags like `-nv`; every char must be a known toggle.
            other if other.starts_with('-') && !other.starts_with("--") && other.len() > 1 => {
                for c in other[1..].chars() {
                    match c {
                        'n' => o.dry_run = true,
                        'v' => o.verbose = true,
                        'f' => o.force = true,
                        'A' => o.addremove = Some(true),
                        'u' => o.update = true,
                        'N' => o.intent_to_add = true,
                        'p' => bail!("interactive patch mode (-p/--patch) is not supported"),
                        'i' => bail!("interactive mode (-i/--interactive) is not supported"),
                        'e' => bail!("--edit is not supported"),
                        _ => bail!("unsupported flag -{c}"),
                    }
                }
            }
            other if other.starts_with("--") => bail!("unsupported flag {other}"),
            _ => o.pathspecs.push(a.clone()),
        }
    }
    Ok(o)
}

// ---------------------------------------------------------------------------
// entry point
// ---------------------------------------------------------------------------

pub fn stage(args: &[String]) -> Result<ExitCode> {
    let o = parse(args)?;

    // --- option validation, in git's own order ------------------------------
    // The order matters when an invocation violates several rules at once, and it
    // is not argv order. Verified against git 2.55.0, highest precedence first:
    // the `-A`/`-u` conflict, then `--ignore-missing` without `--dry-run`, then an
    // empty-string pathspec, then `--pathspec-file-nul` without its file.
    if o.addremove == Some(true) && o.update {
        eprintln!("fatal: options '-A' and '-u' cannot be used together");
        return Ok(ExitCode::from(FATAL));
    }
    if o.ignore_missing && !o.dry_run {
        eprintln!("fatal: the option '--ignore-missing' requires '--dry-run'");
        return Ok(ExitCode::from(FATAL));
    }
    if o.pathspecs.iter().any(String::is_empty) {
        eprintln!("fatal: empty string is not a valid pathspec. please use . instead if you meant to match all paths");
        return Ok(ExitCode::from(FATAL));
    }
    if o.pathspec_file_nul && !o.pathspec_from_file {
        eprintln!("fatal: the option '--pathspec-file-nul' requires '--pathspec-from-file'");
        return Ok(ExitCode::from(FATAL));
    }
    if o.pathspec_from_file {
        bail!("--pathspec-from-file is not supported");
    }

    let repo = gix::discover(".")?;
    if repo.workdir().is_none() {
        bail!("this operation must be run in a work tree");
    }

    reject_unsupportable_config(&repo, &o)?;

    // Only `-A` and `-u` imply a pathspec; every other flag alone is a no-op.
    if o.pathspecs.is_empty() && !(o.addremove == Some(true) || o.update) {
        eprintln!("Nothing specified, nothing added.");
        eprintln!("hint: Maybe you wanted to say 'git add .'?");
        eprintln!("hint: Disable this message with \"git config set advice.addEmptyPathspec false\"");
        return Ok(ExitCode::SUCCESS);
    }

    if o.refresh {
        return refresh(&repo, &o);
    }
    add(&repo, &o)
}

/// Reject the configurations under which a flag we otherwise accept would
/// silently produce the wrong index, rather than pretending to honour it.
fn reject_unsupportable_config(repo: &gix::Repository, o: &Opts) -> Result<()> {
    let cfg = repo.config_snapshot();

    if o.sparse.is_some() && cfg.boolean("core.sparseCheckout").unwrap_or(false) {
        bail!("--sparse/--no-sparse is not supported in a sparse-checkout repository");
    }

    // `--renormalize` exists only to re-run content filters, and this port applies
    // none. Accepting it where a filter could engage would silently write the
    // unconverted bytes, so refuse whenever conversion is configurable at all.
    if o.renormalize {
        if cfg.string("core.attributesFile").is_some() {
            bail!("--renormalize is not supported: core.attributesFile configures content conversion");
        }
        if let Some(raw) = cfg.string("core.autocrlf") {
            let value = raw.to_str_lossy().trim().to_ascii_lowercase();
            if !matches!(value.as_str(), "false" | "0" | "no" | "off" | "") {
                bail!("--renormalize is not supported: core.autocrlf={value} configures content conversion");
            }
        }
        if cfg.string("core.eol").is_some() {
            bail!("--renormalize is not supported: core.eol configures content conversion");
        }
        if repo.common_dir().join("info").join("attributes").exists() {
            bail!("--renormalize is not supported: $GIT_DIR/info/attributes configures content conversion");
        }
        // A `.gitattributes` at any depth can drive conversion, so check the whole
        // tracked set rather than just the worktree root.
        let index = open_index(repo)?;
        let backing = index.path_backing();
        if index
            .entries()
            .iter()
            .any(|e| {
                let p = e.path_in(backing);
                p == ".gitattributes" || p.ends_with_str(b"/.gitattributes")
            })
        {
            bail!("--renormalize is not supported: .gitattributes configures content conversion");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

fn open_index(repo: &gix::Repository) -> Result<gix::index::File> {
    Ok(if repo.index_path().exists() {
        repo.open_index()?
    } else {
        gix::index::File::from_state(gix::index::State::new(repo.object_hash()), repo.index_path())
    })
}

/// True for the pathspecs that mean "everything at the current prefix": `.`,
/// `./`, `./.`. Anything carrying magic (a leading `:`) is left alone.
fn denotes_prefix_dir(spec: &str) -> bool {
    let trimmed = spec.trim_end_matches('/');
    !trimmed.is_empty() && trimmed.split('/').all(|c| c == ".")
}

/// The pathspec strings to hand to gix, with `.` rewritten into a form gix can
/// actually match.
///
/// gix derives one search-wide common prefix from the glob text of the patterns
/// and then requires every candidate path to start with it
/// (gix-pathspec/src/search/init.rs:60, search/matching.rs:41). Normalizing `.`
/// leaves its path as the literal `"."` (gix-pathspec/src/pattern.rs:110), so the
/// common prefix becomes `"."` and no worktree path can ever clear that check —
/// `git stage .` then behaves as if every pathspec matched nothing. git resolves
/// `.` to "the directory I was run in", so state that directly instead: the
/// all-matching nil spec `:` at the top of the worktree, and an explicit
/// directory spec below it.
fn pathspec_patterns(repo: &gix::Repository, o: &Opts) -> Result<Vec<BString>> {
    let prefix = repo.prefix()?.unwrap_or_else(|| std::path::Path::new(""));
    let prefix = gix::path::to_unix_separators_on_windows(gix::path::into_bstr(prefix)).into_owned();

    Ok(o.pathspecs
        .iter()
        .map(|s| {
            if !denotes_prefix_dir(s) {
                return BString::from(s.as_bytes());
            }
            if prefix.is_empty() {
                return BString::from(":");
            }
            // `:(top)` keeps gix from prepending the prefix a second time.
            let mut out = BString::from(":(top)");
            out.extend_from_slice(&prefix);
            out.push(b'/');
            out
        })
        .collect())
}

/// A pathspec that carries `:(exclude)`/`:!` magic never has to match anything,
/// so it is exempt from the "did not match any files" check.
fn is_exclude_spec(spec: &str) -> bool {
    spec.starts_with(":!")
        || spec.starts_with(":^")
        || (spec.starts_with(":(") && spec[..spec.find(')').unwrap_or(0)].contains("exclude"))
}

/// True when the pathspec is a plain path with no magic and no wildcard, which is
/// the only form for which git reports the gitignore / not-known-to-git errors
/// instead of the generic "did not match any files".
fn is_literal_spec(spec: &str) -> bool {
    !spec.is_empty() && !spec.starts_with(':') && !spec.contains(['*', '?', '['])
}

/// Mark every positive pathspec that matches at least one of `paths` as seen.
///
/// git marks a pathspec seen the moment it matches any examined path on its own —
/// before exclude pathspecs are applied, and regardless of whether another
/// pathspec also matched that path. gix's combined matcher instead attributes each
/// path to a single pathspec and never yields a path an exclude pathspec shadowed,
/// so it under-reports overlapping specs (`src/ src/lib.rs` both matching
/// `src/lib.rs`) and exclude-shadowed specs (`*.md` whose only match is dropped by
/// `:(exclude)README.md`). Recover the rest by testing each still-unseen positive
/// pathspec against `paths` with its own single-pattern matcher, which carries no
/// exclude and so matches exactly what git counts. `paths` is the universe of
/// tracked and to-be-staged paths — never a gitignored-and-skipped one, so a
/// wildcard whose only match is gitignored still (correctly) stays unseen.
fn mark_seen_per_spec(
    repo: &gix::Repository,
    index: &gix::index::File,
    patterns: &[BString],
    o: &Opts,
    paths: &[BString],
    seen: &mut HashSet<usize>,
) -> Result<()> {
    // `patterns` is 1:1 with `o.pathspecs`, so the index doubles as the seen key.
    for (i, spec) in o.pathspecs.iter().enumerate() {
        if seen.contains(&i) || is_exclude_spec(spec) || spec.is_empty() {
            continue;
        }
        let mut ps = repo.pathspec(
            true,
            std::slice::from_ref(&patterns[i]),
            false,
            index,
            gix::worktree::stack::state::attributes::Source::IdMapping,
        )?;
        if paths.iter().any(|p| ps.is_included(p.as_bstr(), Some(false))) {
            seen.insert(i);
        }
    }
    Ok(())
}

/// Report the first pathspec that matched nothing, using the message git uses for
/// the mode we are in. Returns `None` when every pathspec was accounted for.
///
/// `seen` holds the sequence numbers handed out by the pathspec search, which are
/// indices into `o.pathspecs` in argv order.
/// `update_mode` is `-u` outside `--refresh`: that is the only mode in which git
/// swaps the fatal for its "known to git" wording. `refresh()` passes `false`
/// because its own check always uses the plain message.
fn unmatched_pathspec_exit(
    repo: &gix::Repository,
    o: &Opts,
    seen: &HashSet<usize>,
    update_mode: bool,
) -> Option<ExitCode> {
    // `--ignore-missing` (only legal with `--dry-run`) turns the check off.
    if o.ignore_missing {
        return None;
    }
    // Literal pathspecs that exist on disk yet matched nothing. In add mode the
    // only way that happens is a .gitignore exclusion, which git reports as a
    // block listing every such path at once — but only after the loop below has
    // had its chance to die, since the fatal outranks it in either argv order.
    let mut ignored: BTreeSet<&str> = BTreeSet::new();

    for (i, spec) in o.pathspecs.iter().enumerate() {
        if seen.contains(&i) || is_exclude_spec(spec) || spec.is_empty() {
            continue;
        }
        let on_disk = repo
            .workdir_path(BStr::new(spec.as_bytes()))
            .is_some_and(|abs| std::fs::symlink_metadata(abs).is_ok());

        if !on_disk || !is_literal_spec(spec) {
            eprintln!("fatal: pathspec '{spec}' did not match any files");
            return Some(ExitCode::from(FATAL));
        }
        if update_mode {
            // Present on disk but not tracked, and `-u` only touches tracked paths.
            eprintln!("error: pathspec '{spec}' did not match any file(s) known to git");
            return Some(ExitCode::from(FATAL));
        }
        ignored.insert(spec.as_str());
    }

    if !ignored.is_empty() {
        eprintln!("The following paths are ignored by one of your .gitignore files:");
        for p in &ignored {
            eprintln!("{p}");
        }
        eprintln!("hint: Use -f if you really want to add them.");
        eprintln!("hint: Disable this message with \"git config set advice.addIgnoredFile false\"");
        return Some(ExitCode::FAILURE);
    }
    None
}

// ---------------------------------------------------------------------------
// --refresh
// ---------------------------------------------------------------------------

/// `--refresh` re-stats the matched *index* entries and stages nothing. With
/// `--verbose` git switches `refresh_index()` into porcelain mode, which prints
/// a header plus one `M`/`D` line per still-unstaged path on stdout.
fn refresh(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    let index = open_index(repo)?;
    let patterns = pathspec_patterns(repo, o)?;

    let mut ps = repo.pathspec(
        true,
        &patterns,
        false,
        &index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;

    let mut seen: HashSet<usize> = HashSet::new();
    // path -> refreshed stat, applied after the immutable scan.
    let mut restat: HashMap<BString, Stat> = HashMap::new();
    let mut unstaged: BTreeMap<BString, char> = BTreeMap::new();
    // Every refreshable index path, for the same per-spec seen accounting `add`
    // does — an entry a pathspec matches counts even if the combined matcher
    // attributed it to a different, overlapping pathspec.
    let mut universe: Vec<BString> = Vec::new();

    {
        let backing = index.path_backing();
        for e in index.entries() {
            if e.stage() != Stage::Unconflicted || e.mode == Mode::COMMIT {
                continue; // conflicted stages and gitlinks are not refreshable here
            }
            let path = e.path_in(backing);
            universe.push(path.to_owned());
            let Some(m) = ps.pattern_matching_relative_path(path, Some(false)) else {
                continue;
            };
            if m.is_excluded() {
                continue;
            }
            if m.sequence_number < patterns.len() {
                seen.insert(m.sequence_number);
            }

            let owned = path.to_owned();
            let Some(abs) = repo.workdir_path(path) else {
                continue;
            };
            let Ok(md) = gix::index::fs::Metadata::from_path_no_follow(&abs) else {
                unstaged.insert(owned, 'D');
                continue;
            };
            match read_worktree_blob(repo, &abs, &md) {
                Ok((id, mode)) if id == e.id && mode == e.mode => {
                    // Content is unchanged; adopt the fresh stat so later commands
                    // can take the lstat shortcut. Recording only genuine changes
                    // keeps a fully up-to-date index from being rewritten at all.
                    match Stat::from_fs(&md) {
                        Ok(stat) if stat != e.stat => {
                            restat.insert(owned, stat);
                        }
                        _ => {}
                    }
                }
                // Content differs, or could not be read: either way the path still
                // carries an unstaged modification.
                Ok(_) | Err(_) => {
                    unstaged.insert(owned, 'M');
                }
            }
        }
    }

    mark_seen_per_spec(repo, &index, &patterns, o, &universe, &mut seen)?;
    if let Some(code) = unmatched_pathspec_exit(repo, o, &seen, false) {
        return Ok(code);
    }

    if o.verbose && !unstaged.is_empty() {
        println!("Unstaged changes after refreshing the index:");
        for (path, kind) in &unstaged {
            println!("{kind}\t{path}");
        }
    }

    if !o.dry_run && !restat.is_empty() {
        let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
        let mut index = open_index(repo)?;
        for (entry, path) in index.entries_mut_with_paths() {
            if let Some(stat) = restat.get(&path.to_owned()) {
                entry.stat = *stat;
            }
        }
        index.write(gix::index::write::Options::default())?;
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// the staging path
// ---------------------------------------------------------------------------

/// Read a worktree path and return the blob id its content hashes to, plus the
/// index mode it should carry. Nothing is written to the object database — the
/// caller decides that, so `--dry-run` can stay side-effect free.
fn read_worktree_blob(
    repo: &gix::Repository,
    abs: &std::path::Path,
    md: &gix::index::fs::Metadata,
) -> Result<(gix::hash::ObjectId, Mode)> {
    let (bytes, mode) = read_worktree_bytes(abs, md)?;
    let id = gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, &bytes)?;
    Ok((id, mode))
}

fn read_worktree_bytes(
    abs: &std::path::Path,
    md: &gix::index::fs::Metadata,
) -> Result<(Vec<u8>, Mode)> {
    if md.is_symlink() {
        let target = std::fs::read_link(abs)?;
        #[cfg(unix)]
        let bytes = {
            use std::os::unix::ffi::OsStrExt;
            target.as_os_str().as_bytes().to_vec()
        };
        #[cfg(not(unix))]
        let bytes = target.to_string_lossy().into_owned().into_bytes();
        Ok((bytes, Mode::SYMLINK))
    } else {
        let bytes = std::fs::read(abs)?;
        let mode = if md.is_executable() {
            Mode::FILE_EXECUTABLE
        } else {
            Mode::FILE
        };
        Ok((bytes, mode))
    }
}

/// A worktree path that will be written into the index.
struct Staged {
    path: BString,
    id: gix::hash::ObjectId,
    mode: Mode,
    stat: Stat,
    /// Intent-to-add entries carry the empty blob and the `INTENT_TO_ADD` flag.
    intent: bool,
}

fn add(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    let index = open_index(repo)?;

    // Repo-relative stage-0 entries: the tracked set, with what is staged today.
    let tracked: HashMap<BString, (gix::hash::ObjectId, Mode)> = {
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .filter(|e| e.stage() == Stage::Unconflicted)
            .map(|e| (e.path_in(backing).to_owned(), (e.id, e.mode)))
            .collect()
    };

    let patterns = pathspec_patterns(repo, o)?;

    // --- directory walk over the worktree, filtered by the pathspecs --------
    // Ignored entries are emitted too so a path that is both tracked and
    // gitignored can still be restaged; they are filtered right below.
    let options = repo
        .dirwalk_options()?
        .emit_tracked(true)
        .emit_ignored(Some(gix::dir::walk::EmissionMode::Matching));
    let dirwalk_index = repo.index_or_load_from_head_or_empty()?;
    let mut iter = repo.dirwalk_iter(dirwalk_index, patterns.clone(), Default::default(), options)?;

    // (path, was-ignored) for every stageable worktree file the walk turned up.
    let mut candidates: Vec<(BString, bool)> = Vec::new();
    for item in iter.by_ref() {
        let entry = item?.entry;
        match entry.disk_kind {
            Some(gix::dir::entry::Kind::File) | Some(gix::dir::entry::Kind::Symlink) => {}
            _ => continue, // directories, submodule repositories, untrackable things
        }
        let is_ignored = matches!(entry.status, gix::dir::entry::Status::Ignored(_));
        candidates.push((entry.rela_path, is_ignored));
    }
    drop(iter);

    // A second, independent matcher: unlike the walk's, this one reports *which*
    // pathspec matched, which is what drives the "did not match any files" check
    // and, in turn, makes directory specs like `src/` resolve correctly.
    let mut ps = repo.pathspec(
        true,
        &patterns,
        false,
        &index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;
    // Sequence numbers are indices into `patterns` in argv order; the synthetic
    // match an exclude-only pathspec set produces carries `patterns.len()`
    // (gix-pathspec/src/search/matching.rs:114), so it is filtered out here.
    let mut seen: HashSet<usize> = HashSet::new();

    // --- decide what each candidate becomes ---------------------------------
    let mut staged: Vec<Staged> = Vec::new();
    let mut printed: BTreeMap<BString, &'static str> = BTreeMap::new();
    let mut had_error = false;
    // The paths git counts toward "did the pathspec match anything": everything
    // that cleared the ignore/update filters below, plus the tracked set added
    // after the loop. Fed to `mark_seen_per_spec` so overlapping and
    // exclude-shadowed pathspecs are attributed the way git attributes them.
    let mut universe: Vec<BString> = Vec::new();

    for (path, is_ignored) in candidates {
        let current = tracked.get(&path);
        let already_tracked = current.is_some();

        // An ignored path is only staged when forced or already tracked, and an
        // ignored-and-skipped path deliberately does NOT mark its pathspec seen —
        // that is what makes `git stage <ignored>` report the gitignore error.
        if is_ignored && !o.force && !already_tracked {
            continue;
        }
        // `-u/--update` restages tracked paths only; brand-new files are not its business.
        if o.update && !already_tracked {
            continue;
        }
        universe.push(path.clone());

        if let Some(m) = ps.pattern_matching_relative_path(path.as_bstr(), Some(false)) {
            if !m.is_excluded() && m.sequence_number < patterns.len() {
                seen.insert(m.sequence_number);
            }
        }

        let Some(abs) = repo.workdir_path(&path) else {
            continue;
        };
        let md = match gix::index::fs::Metadata::from_path_no_follow(&abs) {
            Ok(md) => md,
            Err(e) => {
                report_read_error(path.as_ref(), &e, &mut had_error);
                continue;
            }
        };

        // `-N` records untracked paths as the empty blob and reports every matched
        // path as added, leaving already-tracked entries untouched.
        if o.intent_to_add {
            printed.insert(path.clone(), "add");
            if !already_tracked {
                let stat = Stat::from_fs(&md).unwrap_or_default();
                staged.push(Staged {
                    path,
                    id: repo.object_hash().empty_blob(),
                    mode: Mode::FILE,
                    stat,
                    intent: true,
                });
            }
            continue;
        }

        let (id, mode) = match read_worktree_blob(repo, &abs, &md) {
            Ok(v) => v,
            Err(e) => {
                report_read_error(path.as_ref(), &e, &mut had_error);
                continue;
            }
        };

        // Unchanged content and mode: nothing to report, nothing to write. This is
        // what keeps `--verbose` quiet for paths git would leave alone.
        if current == Some(&(id, mode)) {
            continue;
        }
        printed.insert(path.clone(), "add");
        let stat = Stat::from_fs(&md).unwrap_or_default();
        staged.push(Staged { path, id, mode, stat, intent: false });
    }

    // --- deletions: tracked stage-0 paths, matched, whose file is gone ------
    let staged_set: HashSet<BString> = staged.iter().map(|s| s.path.clone()).collect();
    let mut deletions: Vec<BString> = Vec::new();
    {
        let backing = index.path_backing();
        for e in index.entries() {
            if e.stage() != Stage::Unconflicted || e.mode == Mode::COMMIT {
                continue; // leave conflicted stages and submodule gitlinks alone
            }
            let path = e.path_in(backing);
            let owned = path.to_owned();
            if staged_set.contains(&owned) || printed.contains_key(&owned) {
                continue;
            }
            match ps.pattern_matching_relative_path(path, Some(false)) {
                Some(m) if !m.is_excluded() => {
                    let gone = match repo.workdir_path(path) {
                        Some(p) => std::fs::symlink_metadata(p).is_err(),
                        None => true,
                    };
                    // A tracked path that still exists was handled by the walk; a
                    // vanished one marks its pathspec seen either way, because git
                    // considers a removal a match.
                    if m.sequence_number < patterns.len() {
                        seen.insert(m.sequence_number);
                    }
                    if gone && (o.stages_deletions() || o.update) {
                        deletions.push(owned.clone());
                        printed.insert(owned, "remove");
                    }
                }
                _ => continue,
            }
        }
    }

    // --- validate that every pathspec matched something ---------------------
    // Runs before any object or index write, matching git: a bad pathspec leaves
    // the repository, and the object database, completely untouched. The tracked
    // set joins the walked candidates so a pathspec that matched only a tracked
    // (possibly exclude-shadowed) path is still recorded as seen.
    universe.extend(tracked.keys().cloned());
    mark_seen_per_spec(repo, &index, &patterns, o, &universe, &mut seen)?;
    if let Some(code) = unmatched_pathspec_exit(repo, o, &seen, o.update) {
        return Ok(code);
    }

    // An unreadable file is only survivable under `--ignore-errors`; otherwise git
    // reports every failure it hit and then dies once, after the scan.
    if had_error && !o.ignore_errors {
        eprintln!("fatal: adding files failed");
        return Ok(ExitCode::from(FATAL));
    }

    // --- dry run: report only, never touch the index or the odb -------------
    if o.dry_run {
        report(&printed);
        return Ok(exit_status(had_error));
    }

    // Nothing to write: report and leave the index file alone, so its extensions
    // (notably the tree cache dropped below) survive a run that changed nothing.
    if staged.is_empty() && deletions.is_empty() && o.chmod.is_none() {
        if o.verbose {
            report(&printed);
        }
        return Ok(exit_status(had_error));
    }

    // --- write path: serialize the read-modify-write through the coordinator.
    // Hold the lock across a FRESH re-read of the on-disk index and the write, so
    // a concurrent writer's changes to other paths are not clobbered — only the
    // paths this invocation touches are replaced.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let mut index = open_index(repo)?;

    // Write the blobs only now — after the pathspec check has passed — so a bad
    // pathspec leaves the object database byte-identical, the way git does.
    // The id recorded in the index is the one the write returned, so the two can
    // never disagree even if the file changed since the scan hashed it.
    for s in &mut staged {
        s.id = if s.intent {
            repo.write_blob(b"")?.detach()
        } else {
            let abs = repo.workdir_path(&s.path).expect("path came from this worktree");
            let md = gix::index::fs::Metadata::from_path_no_follow(&abs)?;
            let (bytes, mode) = read_worktree_bytes(&abs, &md)?;
            s.mode = mode;
            repo.write_blob(&bytes)?.detach()
        };
    }

    // Drop every prior version (any stage) of a staged path and every deletion,
    // then append the fresh stage-0 entries and restore sort order.
    let remove: HashSet<BString> = staged_set.iter().cloned().chain(deletions.iter().cloned()).collect();
    index.remove_entries(|_, path, _| remove.contains(&path.to_owned()));
    for s in &staged {
        // `EXTENDED` has to accompany `INTENT_TO_ADD` or the flag is dropped on
        // write: gix only emits the extended-flag word, and picks index v3, when
        // that bit is set (gix-index/src/entry/write.rs:27, write.rs:148).
        let flags = if s.intent {
            Flags::INTENT_TO_ADD | Flags::EXTENDED
        } else {
            Flags::empty()
        };
        index.dangerously_push_entry(s.stat, s.id, flags, s.mode, s.path.as_ref());
    }
    index.sort_entries();

    // `--chmod` forces the mode of every matched path, whether or not this run
    // restaged it, and never contributes to the verbose report.
    if let Some(executable) = o.chmod {
        let want = if executable { Mode::FILE_EXECUTABLE } else { Mode::FILE };
        // Collect first, so the matcher and the index's shared borrow are both
        // released before the entries are mutated below.
        let wanted: HashSet<BString> = {
            let mut matcher = repo.pathspec(
                true,
                &patterns,
                false,
                &index,
                gix::worktree::stack::state::attributes::Source::IdMapping,
            )?;
            let backing = index.path_backing();
            index
                .entries()
                .iter()
                .filter(|e| {
                    e.stage() == Stage::Unconflicted
                        && (e.mode == Mode::FILE || e.mode == Mode::FILE_EXECUTABLE)
                })
                .map(|e| e.path_in(backing).to_owned())
                .filter(|p| matcher.is_included(p.as_bstr(), Some(false)))
                .collect()
        };
        for (entry, path) in index.entries_mut_with_paths() {
            if wanted.contains(&path.to_owned()) {
                entry.mode = want;
            }
        }
    }

    // The tree-cache extension is written verbatim by `File::write`; drop it after
    // mutating entries so a later commit can't capture a stale subtree.
    index.remove_tree();
    index.write(gix::index::write::Options::default())?;

    if o.verbose {
        report(&printed);
    }
    Ok(exit_status(had_error))
}

/// git prints both of these lines for each unreadable file and keeps going; only
/// after the whole run does it decide between dying (128) and, under
/// `--ignore-errors`, reporting the skips with exit 1.
fn report_read_error(path: &BStr, err: &dyn std::fmt::Display, had_error: &mut bool) {
    eprintln!("error: open(\"{path}\"): {err}");
    eprintln!("error: unable to index file '{path}'");
    *had_error = true;
}

fn exit_status(had_error: bool) -> ExitCode {
    if had_error {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// git reports staged paths in index order — a plain byte-wise sort — with adds
/// and removes interleaved, not grouped.
fn report(printed: &BTreeMap<BString, &'static str>) {
    for (path, kind) in printed {
        println!("{kind} '{path}'");
    }
}
