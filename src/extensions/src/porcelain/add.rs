//! `git add` — stage worktree paths into the index, served natively via the
//! vendored gitoxide crates so tools on PATH see the same staged index.
//!
//! Supported forms (the dominant `git add` invocations):
//!   * `git add <pathspec>...`  — stage files/dirs (recurses, honors `.gitignore`)
//!   * `git add .`              — stage everything under the current prefix
//!   * `git add -A|--all`       — stage the whole worktree (adds, mods, deletes)
//!   * `git add -u|--update`    — restage tracked paths only (mods + deletes)
//!   * `git add -N|--intent-to-add` — record that untracked paths will be added
//!   * `git add --chmod=(+|-)x` — override the executable bit of staged files
//!   * `git add --refresh`      — refresh the stat cache, do not add content
//!   * `git add --renormalize`  — restage tracked paths (implies -u)
//!   * `git add --pathspec-from-file=<f>` (`-` = stdin, `--pathspec-file-nul`)
//!   * `git add --ignore-removal|--no-all` — do not stage worktree deletions
//!   * `git add --ignore-errors` — skip files that cannot be read, exit 1
//!     (default from the `add.ignoreErrors` / `add.ignore-errors` config key)
//!   * `git add --ignore-missing` — with `-n`, tolerate non-matching pathspecs
//!   * flags `-f/--force`, `-n/--dry-run`, `-v/--verbose`, `--sparse`, `--`, and
//!     `--warn-embedded-repo`/`--no-warn-embedded-repo` (accepted no-op: the
//!     embedded-repo warning is moot since gitlinks are never staged here)
//!
//! For each matched worktree file the blob is hashed into the object database and
//! its index entry is (re)written with the current mode and filesystem stat.
//! Tracked paths whose worktree file is gone are staged as deletions, matching
//! modern `git add` semantics. Unmerged (conflicted) entries under a matched path
//! are collapsed to the freshly-staged stage-0 entry.
//!
//! Deviations (bailed or noted, never faked):
//!   * `.gitattributes` content filters (autocrlf, `clean`/`smudge`) are NOT
//!     applied — the blob is the verbatim worktree bytes. `--renormalize` therefore
//!     re-stages current bytes without re-running EOL filters.
//!   * submodule gitlinks are skipped here (use `git zbump`).
//!   * interactive/patch/edit modes are rejected — they require a TTY here.
//!   * `-U/--unified`, `--inter-hunk-context`, `--[no-]auto-advance` only configure
//!     the interactive/patch diff. Their values are magnitude-validated exactly as
//!     git's `OPT_MAGNITUDE` (bad value ⇒ exit 129), then — since patch mode is
//!     never entered here — git's `fatal: the option '<x>' requires
//!     '--interactive/--patch'` (exit 128) is reproduced. A bare `--auto-advance`
//!     is the default and stages normally; only `--no-auto-advance` triggers it.

use anyhow::{bail, Result};
use std::collections::HashSet;
use std::io::Read;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::index::entry::{Flags, Mode, Stage, Stat};

pub fn add(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    if repo.workdir().is_none() {
        bail!("this operation must be run in a work tree");
    }

    // --- argument parse -----------------------------------------------------
    let mut dry_run = false;
    let mut verbose = false;
    let mut force = false;
    let mut all = false;
    let mut update_only = false;
    let mut intent_to_add = false;
    let mut refresh = false;
    let mut renormalize = false;
    // `add.ignoreErrors` (alias `add.ignore-errors`) is the default for
    // `--ignore-errors`; the explicit `--ignore-errors`/`--no-ignore-errors`
    // flags parsed below override it, matching git's config-then-CLI precedence.
    let mut ignore_errors = {
        let cfg = repo.config_snapshot();
        cfg.boolean("add.ignoreErrors")
            .or_else(|| cfg.boolean("add.ignore-errors"))
            .unwrap_or(false)
    };
    let mut ignore_missing = false;
    // `--no-all`/`--ignore-removal`: stage adds+mods but not worktree deletions.
    let mut no_removal = false;
    // Some(true) => `--chmod=+x`, Some(false) => `--chmod=-x`.
    let mut chmod: Option<bool> = None;
    let mut from_file: Option<String> = None;
    let mut file_nul = false;
    // Interactive/patch-only diff options: `-U`/`--unified`, `--inter-hunk-context`,
    // and `--[no-]auto-advance`. Only whether each was set (and, for auto-advance,
    // whether it was turned *off*) matters here — see the post-loop requires check.
    let mut unified_seen = false;
    let mut interhunk_seen = false;
    let mut auto_advance: Option<bool> = None;
    let mut pathspecs: Vec<String> = Vec::new();
    let mut positional_only = false;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if positional_only {
            pathspecs.push(a.clone());
            i += 1;
            continue;
        }
        match a.as_str() {
            "--" => positional_only = true,
            "-n" | "--dry-run" => dry_run = true,
            "--no-dry-run" => dry_run = false,
            "-v" | "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            "-f" | "--force" => force = true,
            "--no-force" => force = false,
            "-A" | "--all" | "--no-ignore-removal" => {
                all = true;
                no_removal = false;
            }
            "--no-all" | "--ignore-removal" => {
                all = false;
                no_removal = true;
            }
            "-u" | "--update" => update_only = true,
            "--no-update" => update_only = false,
            "-N" | "--intent-to-add" => intent_to_add = true,
            "--no-intent-to-add" => intent_to_add = false,
            "--refresh" => refresh = true,
            // `--renormalize` re-stages tracked paths (implies -u). Content filters
            // are not applied here, so it restages the verbatim worktree bytes.
            "--renormalize" => renormalize = true,
            "--no-renormalize" => renormalize = false,
            "--sparse" | "--no-sparse" => { /* no sparse-checkout cone here: accept and ignore */ }
            // `--warn-embedded-repo`/`--no-warn-embedded-repo` (hidden in git,
            // builtin/add.c:393 `OPT_HIDDEN_BOOL`, default on) only toggles the
            // `adding embedded git repository:` warning that git's
            // `check_embedded_repo` emits when a matched worktree directory is
            // itself a git repo. This port never stages gitlinks/embedded repos
            // (the walk keeps only File/Symlink entries), so no such warning is
            // ever produced and both toggles are accepted no-ops — matching git's
            // exit-0 accept for the non-embedded-repo case.
            "--warn-embedded-repo" | "--no-warn-embedded-repo" => {}
            "--ignore-errors" => ignore_errors = true,
            "--no-ignore-errors" => ignore_errors = false,
            "--ignore-missing" => ignore_missing = true,
            "--no-ignore-missing" => ignore_missing = false,
            "--pathspec-file-nul" => file_nul = true,
            "--no-pathspec-file-nul" => file_nul = false,
            // Value-taking flags: accept both `--flag=value` and `--flag value`.
            "--chmod" => {
                i += 1;
                let v = args.get(i).map(String::as_str).unwrap_or("");
                match parse_chmod(v) {
                    Some(b) => chmod = Some(b),
                    None => return usage_fatal(format!("--chmod param '{v}' must be either -x or +x")),
                }
            }
            s if s.starts_with("--chmod=") => match parse_chmod(&s["--chmod=".len()..]) {
                Some(b) => chmod = Some(b),
                None => {
                    let v = &s["--chmod=".len()..];
                    return usage_fatal(format!("--chmod param '{v}' must be either -x or +x"));
                }
            },
            "--pathspec-from-file" => {
                i += 1;
                from_file = Some(args.get(i).cloned().unwrap_or_default());
            }
            s if s.starts_with("--pathspec-from-file=") => {
                from_file = Some(s["--pathspec-from-file=".len()..].to_string());
            }
            // Interactive modes need a TTY / editor that does not exist here.
            "-p" | "--patch" => bail!("interactive patch mode (-p/--patch) is not ported"),
            "-i" | "--interactive" => bail!("interactive mode (-i/--interactive) is not ported"),
            "-e" | "--edit" => bail!("edit mode (-e/--edit) needs an interactive editor; not ported"),
            // `-U`/`--unified`, `--inter-hunk-context`, and `--[no-]auto-advance`
            // exist only to configure the interactive/patch diff. git parses (and
            // magnitude-validates) their values here but only consumes them once
            // `--patch`/`--interactive` runs, dying otherwise (checked after the
            // loop). The value is discarded; only its validity, and the fact the
            // option was given, are observable. `-U`/`--unified` take a separate
            // value; `-U` reports itself as a short `switch`, the long forms as
            // `option`. git always consumes the next argv element as the value
            // (so `-U -A` treats `-A` as the value), matching `--chmod` above.
            "-U" | "--unified" => {
                let short = a == "-U";
                let name = if short { "U" } else { "unified" };
                i += 1;
                if let Err(code) = check_magnitude(args.get(i).map(String::as_str), short, name) {
                    return Ok(code);
                }
                unified_seen = true;
            }
            "--inter-hunk-context" => {
                i += 1;
                if let Err(code) =
                    check_magnitude(args.get(i).map(String::as_str), false, "inter-hunk-context")
                {
                    return Ok(code);
                }
                interhunk_seen = true;
            }
            "--auto-advance" => auto_advance = Some(true),
            "--no-auto-advance" => auto_advance = Some(false),
            // Sticky value forms `-U<n>`, `--unified=<n>`, `--inter-hunk-context=<n>`.
            s if s.starts_with("-U") && !s.starts_with("--") && s.len() > 2 => {
                if let Err(code) = check_magnitude(Some(&s[2..]), true, "U") {
                    return Ok(code);
                }
                unified_seen = true;
            }
            s if s.starts_with("--unified=") => {
                if let Err(code) = check_magnitude(Some(&s["--unified=".len()..]), false, "unified") {
                    return Ok(code);
                }
                unified_seen = true;
            }
            s if s.starts_with("--inter-hunk-context=") => {
                if let Err(code) = check_magnitude(
                    Some(&s["--inter-hunk-context=".len()..]),
                    false,
                    "inter-hunk-context",
                ) {
                    return Ok(code);
                }
                interhunk_seen = true;
            }
            // `--[no-]auto-advance` is a pure toggle: a `=value` is a usage error.
            s if s.starts_with("--auto-advance=") => {
                eprintln!("error: option `auto-advance' takes no value");
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with("--no-auto-advance=") => {
                eprintln!("error: option `no-auto-advance' takes no value");
                return Ok(ExitCode::from(129));
            }
            // Bundled short flags like `-nv`; every char must be a known toggle.
            other if other.starts_with('-') && !other.starts_with("--") && other.len() > 1 => {
                for c in other[1..].chars() {
                    match c {
                        'n' => dry_run = true,
                        'v' => verbose = true,
                        'f' => force = true,
                        'A' => all = true,
                        'u' => update_only = true,
                        'N' => intent_to_add = true,
                        _ => return usage_error(format!("unknown switch `{c}'")),
                    }
                }
            }
            other if other.starts_with('-') => return usage_error(format!("unknown option `{}'", other.trim_start_matches('-'))),
            _ => pathspecs.push(a.clone()),
        }
        i += 1;
    }

    // `-U`/`--unified`, `--inter-hunk-context`, and `--no-auto-advance` only feed
    // the interactive/patch diff machinery, which this port does not serve. git
    // collects them into `add_p_opt` and, when neither `--patch` nor `--interactive`
    // is active, dies here — before pathspec setup, `--ignore-missing`, the
    // empty-pathspec check, and even the `-A`/`-u` conflict (verified against git
    // 2.55.0). `-p`/`-i`/`-e` already bailed above, so reaching this point means
    // patch mode is off; reproduce the fatal. The cited option follows git's fixed
    // precedence: `--unified`, then `--inter-hunk-context`, then `--no-auto-advance`
    // (a bare `--auto-advance` is the default and never triggers it).
    if unified_seen || interhunk_seen || auto_advance == Some(false) {
        let opt = if unified_seen {
            "--unified"
        } else if interhunk_seen {
            "--inter-hunk-context"
        } else {
            "--no-auto-advance"
        };
        eprintln!("fatal: the option '{opt}' requires '--interactive/--patch'");
        return Ok(ExitCode::from(128));
    }

    // `--pathspec-from-file`: read pathspecs from a file (or stdin for `-`).
    if let Some(src) = from_file {
        if !pathspecs.is_empty() {
            return usage_fatal(
                "'--pathspec-from-file' and pathspec arguments cannot be used together".into(),
            );
        }
        pathspecs = read_pathspec_file(&src, file_nul)?;
    } else if file_nul {
        return usage_fatal(
            "the option '--pathspec-file-nul' requires '--pathspec-from-file'".into(),
        );
    }

    // `--ignore-missing` is only meaningful with `--dry-run`.
    if ignore_missing && !dry_run {
        return usage_fatal("the option '--ignore-missing' requires '--dry-run'".into());
    }

    // git rejects an empty-string pathspec outright.
    if pathspecs.iter().any(String::is_empty) {
        return usage_fatal(
            "empty string is not a valid pathspec. please use . instead if you meant to match all paths"
                .into(),
        );
    }

    if pathspecs.is_empty() && !(all || update_only) {
        // git: message + advice on stderr, exit 0. stdout stays empty.
        eprintln!("Nothing specified, nothing added.");
        if crate::advice::enabled("addEmptyPathspec") {
            eprintln!("hint: Maybe you wanted to say 'git add .'?");
            eprintln!(
                "hint: Disable this message with \"git config set advice.addEmptyPathspec false\""
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    // Update, refresh, and renormalize all restrict staging to tracked paths.
    let tracked_only = update_only || refresh || renormalize;
    // A real add writes new content blobs. Dry-run, refresh, and intent-to-add
    // never write per-file content objects (git writes none in those modes).
    let write_content = !dry_run && !refresh && !intent_to_add;

    // --- index snapshot: read-only, drives staging decisions and deletions.
    // The authoritative mutation index is re-read under the lock further below.
    let index = if repo.index_path().exists() {
        repo.open_index()?
    } else {
        gix::index::File::from_state(gix::index::State::new(repo.object_hash()), repo.index_path())
    };

    // Repo-relative paths of the current stage-0 entries (tracked set).
    let existing: HashSet<BString> = {
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .filter(|e| e.stage() == Stage::Unconflicted)
            .map(|e| e.path_in(backing).to_owned())
            .collect()
    };

    // A bare `.` / `./` at the repository root is git's "everything under the
    // current directory", i.e. the whole worktree. gitoxide's dirwalk mishandles
    // it there: the pathspec normalizes to a *nil* pattern whose path is the
    // literal `.` (gix-pathspec `Pattern::normalize`), and the walk then
    // prefix-matches `.`, emitting only dot-prefixed entries before stopping. `:/`
    // (match from the repo root) is the equivalent that gitoxide walks correctly.
    // Only rewrite at the root: from a subdirectory `.` normalizes to the prefix
    // path (not nil) and walks fine, so it is left untouched.
    let at_root = repo
        .prefix()
        .ok()
        .flatten()
        .map_or(true, |p| p.as_os_str().is_empty());
    if at_root {
        for spec in pathspecs.iter_mut() {
            if spec == "." || spec == "./" {
                *spec = ":/".to_string();
            }
        }
    }

    // --- directory walk over the worktree, filtered by the pathspecs --------
    // Emit tracked and untracked files individually; also emit ignored ones so a
    // path that is both tracked and gitignored can still be restaged. Ignored
    // entries are only kept when forced or already tracked (decided below).
    let patterns: Vec<BString> = pathspecs
        .iter()
        .map(|s| BString::from(s.clone().into_bytes()))
        .collect();
    let options = repo
        .dirwalk_options()?
        .emit_tracked(true)
        .emit_ignored(Some(gix::dir::walk::EmissionMode::Matching));

    let dirwalk_index = repo.index_or_load_from_head_or_empty()?;
    let mut iter = repo.dirwalk_iter(dirwalk_index, patterns, Default::default(), options)?;

    // A staged entry to be written into the index.
    struct Staged {
        path: BString,
        id: gix::hash::ObjectId,
        mode: Mode,
        stat: Stat,
        was_tracked: bool,
    }
    let mut staged: Vec<Staged> = Vec::new();
    // Paths that could not be read, paired with the OS error text git reports
    // (only surfaced for a real add). git prints `open("<p>"): <strerror>`.
    let mut read_errors: Vec<(BString, String)> = Vec::new();

    for item in iter.by_ref() {
        let entry = item?.entry;
        // Only regular files and symlinks are stageable content; skip directories,
        // submodule repositories, and anything untrackable.
        match entry.disk_kind {
            Some(gix::dir::entry::Kind::File) | Some(gix::dir::entry::Kind::Symlink) => {}
            _ => continue,
        }

        let path = entry.rela_path;
        let already_tracked = existing.contains(&path);

        // Ignore semantics: an ignored path is only staged if forced or already
        // tracked. Tracked/untracked (non-ignored) paths are always eligible.
        if matches!(entry.status, gix::dir::entry::Status::Ignored(_)) && !force && !already_tracked
        {
            continue;
        }
        // `-u/--update`, `--refresh`, `--renormalize` restage tracked paths only.
        if tracked_only && !already_tracked {
            continue;
        }
        // `-N/--intent-to-add` never rewrites the content of already-tracked
        // paths; those are kept in the matched set for reporting but filtered
        // out at write time (only brand-new files get an intent-to-add entry).

        let Some(abs) = repo.workdir_path(&path) else {
            continue;
        };
        let md = gix::index::fs::Metadata::from_path_no_follow(&abs)?;

        let (bytes, mode) = if md.is_symlink() {
            let target = match std::fs::read_link(&abs) {
                Ok(t) => t,
                Err(e) => {
                    read_errors.push((path, os_err_message(&e)));
                    continue;
                }
            };
            #[cfg(unix)]
            let bytes = {
                use std::os::unix::ffi::OsStrExt;
                target.as_os_str().as_bytes().to_vec()
            };
            #[cfg(not(unix))]
            let bytes = target.to_string_lossy().into_owned().into_bytes();
            (bytes, Mode::SYMLINK)
        } else {
            let bytes = match std::fs::read(&abs) {
                Ok(b) => b,
                Err(e) => {
                    read_errors.push((path, os_err_message(&e)));
                    continue;
                }
            };
            let mode = if md.is_executable() {
                Mode::FILE_EXECUTABLE
            } else {
                Mode::FILE
            };
            (bytes, mode)
        };

        // `--chmod=(+|-)x` overrides the executable bit of regular files (not
        // symlinks), for both the object mode and what lands in the index.
        let mode = match (chmod, mode) {
            (Some(true), Mode::FILE) | (Some(true), Mode::FILE_EXECUTABLE) => Mode::FILE_EXECUTABLE,
            (Some(false), Mode::FILE) | (Some(false), Mode::FILE_EXECUTABLE) => Mode::FILE,
            (_, m) => m,
        };

        // Only a real add hashes content into the odb. Other modes still need the
        // blob id (for change detection in the report) but must not create objects,
        // so they compute the hash without writing it.
        let id = if write_content {
            repo.write_blob(&bytes)?.detach()
        } else {
            gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, &bytes)?
        };
        let stat = Stat::from_fs(&md)?;
        staged.push(Staged { path, id, mode, stat, was_tracked: already_tracked });
    }

    // Recover the pathspec matcher (usable without borrowing the repo) to decide
    // deletions and to validate that each explicit pathspec matched something.
    let mut pathspec = match iter.into_outcome() {
        Some(outcome) => outcome.pathspec,
        None => bail!("directory walk did not complete"),
    };

    let staged_set: HashSet<BString> = staged.iter().map(|s| s.path.clone()).collect();

    // --- deletions: tracked stage-0 paths, matched, whose file is gone ------
    // Suppressed by `--no-all`/`--ignore-removal`.
    let mut deletions: Vec<BString> = Vec::new();
    if !no_removal {
        let backing = index.path_backing();
        for e in index.entries() {
            if e.stage() != Stage::Unconflicted || e.mode == Mode::COMMIT {
                continue; // leave conflicted stages and submodule gitlinks alone
            }
            let path = e.path_in(backing);
            let owned = path.to_owned();
            if staged_set.contains(&owned) {
                continue;
            }
            if !pathspec.is_included(path, Some(false)) {
                continue;
            }
            let gone = match repo.workdir_path(path) {
                Some(p) => std::fs::symlink_metadata(p).is_err(),
                None => true,
            };
            if gone {
                deletions.push(owned);
            }
        }
    }

    // --- validate explicit literal pathspecs matched something --------------
    // Mirrors git's `pathspec '<x>' did not match any files` and its refusal to
    // add a gitignored path without `-f`. Magic pathspecs are left to the matcher.
    // `--ignore-missing` (dry-run only) tolerates non-matching pathspecs.
    let deletion_set: HashSet<&BString> = deletions.iter().collect();
    for p in &pathspecs {
        if p == "." || p.is_empty() || p.starts_with(':') || p.contains(['*', '?', '[']) {
            continue;
        }
        let on_disk = repo
            .workdir_path(BStr::new(p.as_bytes()))
            .is_some_and(|abs| std::fs::symlink_metadata(abs).is_ok());
        let matched_staged = path_is_or_under(staged_set.iter(), p);
        let matched_tracked = path_is_or_under(existing.iter(), p);
        let matched_deleted = path_is_or_under(deletion_set.iter().copied(), p);

        if matched_staged || matched_tracked || matched_deleted {
            continue;
        }
        if tracked_only {
            // `-u`/`--refresh`/`--renormalize` only consider tracked paths.
            // `--renormalize` is lenient: an existing untracked/ignored path that
            // matches no tracked entry is a silent no-op. `-u`/`--refresh` and any
            // absent path are "did not match".
            if renormalize && on_disk {
                continue;
            }
            if !ignore_missing {
                eprintln!("fatal: pathspec '{p}' did not match any files");
                return Ok(ExitCode::from(128));
            }
            continue;
        }
        if on_disk && !force {
            // Present on disk but not staged/tracked ⇒ excluded by .gitignore.
            // git: message on stderr, exit 1.
            eprintln!("The following paths are ignored by one of your .gitignore files:");
            eprintln!("{p}");
            if crate::advice::enabled("addIgnoredFile") {
                eprintln!("hint: Use -f if you really want to add them.");
                eprintln!(
                    "hint: Disable this message with \"git config set advice.addIgnoredFile false\""
                );
            }
            return Ok(ExitCode::from(1));
        }
        if !on_disk && !ignore_missing {
            eprintln!("fatal: pathspec '{p}' did not match any files");
            return Ok(ExitCode::from(128));
        }
    }

    // `--refresh` only refreshes the stat cache (invisible to the object/ref/index
    // logical state) and never adds content: nothing more to write here.
    if refresh {
        return Ok(ExitCode::SUCCESS);
    }

    // `--renormalize` re-stages tracked content but refuses to stat a matched
    // tracked path whose worktree file is gone — git aborts with a fatal there
    // rather than staging the removal.
    if renormalize {
        if let Some(first) = deletions.first() {
            eprintln!("fatal: unable to stat '{first}': No such file or directory");
            return Ok(ExitCode::from(128));
        }
    }

    // `--ignore-errors`: a real add reports unreadable files and, if any occurred
    // without `--ignore-errors`, aborts before touching the index.
    if !read_errors.is_empty() && !dry_run {
        for (p, msg) in &read_errors {
            eprintln!("error: open(\"{p}\"): {msg}");
            eprintln!("error: unable to index file '{p}'");
        }
        if !ignore_errors {
            eprintln!("fatal: adding files failed");
            return Ok(ExitCode::from(128));
        }
    }

    // Build the `-n`/`-v` report exactly as git orders it: first the matched
    // tracked entries in index order (a removed file → `remove`, a changed file
    // — or any matched file under `-N` — → `add`, an unchanged file omitted),
    // then the brand-new untracked files in walk order → `add`.
    let report: Vec<String> = if !(dry_run || verbose) {
        Vec::new()
    } else {
        let mut lines = Vec::new();
        let staged_tracked: std::collections::HashMap<&BString, &Staged> =
            staged.iter().filter(|s| s.was_tracked).map(|s| (&s.path, s)).collect();
        let deletion_lookup: HashSet<&BString> = deletions.iter().collect();
        let backing = index.path_backing();
        for e in index.entries() {
            if e.stage() != Stage::Unconflicted || e.mode == Mode::COMMIT {
                continue;
            }
            let path = e.path_in(backing).to_owned();
            if deletion_lookup.contains(&path) {
                lines.push(format!("remove '{path}'"));
            } else if let Some(s) = staged_tracked.get(&path) {
                if intent_to_add || s.id != e.id || s.mode != e.mode {
                    lines.push(format!("add '{path}'"));
                }
            }
        }
        for s in staged.iter().filter(|s| !s.was_tracked) {
            lines.push(format!("add '{}'", s.path));
        }
        lines
    };

    if staged.is_empty() && deletions.is_empty() {
        return Ok(finish_code(&read_errors, ignore_errors, dry_run));
    }

    // --- dry run: report only, never touch the index ------------------------
    if dry_run {
        for line in &report {
            println!("{line}");
        }
        return Ok(finish_code(&read_errors, ignore_errors, dry_run));
    }

    // --- write path: serialize the read-modify-write through the coordinator.
    // Hold the lock across a FRESH re-read of the on-disk index and the write, so
    // a concurrent writer's changes to other paths are not clobbered — only the
    // paths this invocation touches are replaced.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut index = if repo.index_path().exists() {
        repo.open_index()?
    } else {
        gix::index::File::from_state(gix::index::State::new(repo.object_hash()), repo.index_path())
    };

    if intent_to_add {
        // Record intent-to-add: an empty-blob, zero-stat entry with the ITA flag,
        // for untracked matched files only. Tracked paths are left untouched.
        // Deletions are still applied (git stages them for `-N <pathspec>`).
        let ita: Vec<&Staged> = staged.iter().filter(|s| !s.was_tracked).collect();
        let empty_id = if ita.is_empty() {
            repo.object_hash().null()
        } else {
            repo.write_blob(b"")?.detach()
        };
        let remove: HashSet<BString> = ita
            .iter()
            .map(|s| s.path.clone())
            .chain(deletions.iter().cloned())
            .collect();
        index.remove_entries(|_, path, _| remove.contains(&path.to_owned()));
        for s in &ita {
            index.dangerously_push_entry(
                Stat::default(),
                empty_id,
                Flags::EXTENDED | Flags::INTENT_TO_ADD,
                s.mode,
                s.path.as_ref(),
            );
        }
        index.sort_entries();
        index.remove_tree();
        index.write(gix::index::write::Options::default())?;

        if verbose {
            for line in &report {
                println!("{line}");
            }
        }
        return Ok(finish_code(&read_errors, ignore_errors, dry_run));
    }

    // Drop every prior version (any stage) of a staged path and every deletion,
    // then append the fresh stage-0 entries and restore sort order.
    // Files that errored out (only reachable with `--ignore-errors`) never made
    // it into `staged`, so they are naturally skipped here.
    let remove: HashSet<BString> = staged
        .iter()
        .map(|s| s.path.clone())
        .chain(deletions.iter().cloned())
        .collect();
    index.remove_entries(|_, path, _| remove.contains(&path.to_owned()));
    for s in &staged {
        index.dangerously_push_entry(s.stat, s.id, Flags::empty(), s.mode, s.path.as_ref());
    }
    index.sort_entries();

    // The tree-cache extension is written verbatim by `File::write`; drop it after
    // mutating entries so a later commit can't capture a stale subtree.
    index.remove_tree();
    index.write(gix::index::write::Options::default())?;

    if verbose {
        for line in &report {
            println!("{line}");
        }
    }

    Ok(finish_code(&read_errors, ignore_errors, dry_run))
}

/// The overall exit code: git returns 1 from a real add when `--ignore-errors`
/// let it skip at least one unreadable file, else success.
fn finish_code(read_errors: &[(BString, String)], ignore_errors: bool, dry_run: bool) -> ExitCode {
    if ignore_errors && !dry_run && !read_errors.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// The `strerror`-equivalent text git prints for a failed `open()`, e.g.
/// `Permission denied`. Rust renders an OS error as `<strerror> (os error N)`;
/// git shows only the `<strerror>` prefix, so strip the trailing ` (os error N)`.
fn os_err_message(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(idx) => s[..idx].to_string(),
        None => s,
    }
}

/// Validate a `-U`/`--unified`/`--inter-hunk-context` value the way git's
/// `parse_options` does for an `OPT_MAGNITUDE`, emitting git's exact `error:` line
/// and exit 129 on failure. `short` selects the `switch `U'` vs `option `<name>'`
/// wording. `value` is `None` when the option was given with no argument at all.
///
/// git's own error text differs by failure kind (verified against git 2.55.0):
///   * no argument            -> `<label> requires a value`
///   * present but empty       -> `<label> expects a numerical value`
///   * non-empty, not a number -> `<label> expects an integer value with an optional k/m/g suffix`
/// These lines are printed alone — a value error carries no usage block.
fn check_magnitude(value: Option<&str>, short: bool, name: &str) -> std::result::Result<(), ExitCode> {
    let label = if short {
        format!("switch `{name}'")
    } else {
        format!("option `{name}'")
    };
    match value {
        None => eprintln!("error: {label} requires a value"),
        Some("") => eprintln!("error: {label} expects a numerical value"),
        Some(v) if is_valid_magnitude(v) => return Ok(()),
        Some(_) => {
            eprintln!("error: {label} expects an integer value with an optional k/m/g suffix")
        }
    }
    Err(ExitCode::from(129))
}

/// git's `OPT_MAGNITUDE` acceptance test, ported from `git_parse_unsigned` +
/// `get_unit_factor` (config.c): reject any `-`; `strtoumax(_, 0)` must consume at
/// least one digit (base auto-detected — `0x` hex, a leading `0` octal, else
/// decimal); the trailing unit must be empty or one of `k`/`m`/`g` (case-insensitive).
/// The parsed magnitude is discarded here — only its validity is observable — so the
/// overflow-past-`ULONG_MAX` arm (which also fails, with the same exit code) is not
/// reproduced.
fn is_valid_magnitude(s: &str) -> bool {
    if s.contains('-') {
        return false;
    }
    let b = s.as_bytes();
    let mut i = 0;
    // strtoumax skips leading ASCII whitespace and an optional leading `+`.
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if i < b.len() && b[i] == b'+' {
        i += 1;
    }
    // Auto-detect the base the way strtoumax(_, 0) does.
    let (radix, digits_start) = if i + 1 < b.len() && b[i] == b'0' && (b[i + 1] | 0x20) == b'x' {
        (16u32, i + 2)
    } else if i < b.len() && b[i] == b'0' {
        // A lone leading `0` is itself an octal digit, so the run is never empty.
        (8u32, i)
    } else {
        (10u32, i)
    };
    let mut j = digits_start;
    while j < b.len() && (b[j] as char).to_digit(radix).is_some() {
        j += 1;
    }
    if j == digits_start {
        return false; // no digit consumed (e.g. "0x", "abc", "")
    }
    matches!(&s[j..], "" | "k" | "K" | "m" | "M" | "g" | "G")
}

/// `--chmod` value parse: `+x` => `Some(true)`, `-x` => `Some(false)`, else `None`.
fn parse_chmod(v: &str) -> Option<bool> {
    match v {
        "+x" => Some(true),
        "-x" => Some(false),
        _ => None,
    }
}

/// A usage error (git exit 129): unknown option/switch.
fn usage_error(msg: String) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    eprintln!("usage: git add [<options>] [--] <pathspec>...");
    Ok(ExitCode::from(129))
}

/// A fatal argument error (git exit 128).
fn usage_fatal(msg: String) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// Read newline- (or NUL-) separated pathspecs from a file, or from stdin when
/// `src` is `-`. Trailing CR is stripped from newline-separated lines.
fn read_pathspec_file(src: &str, nul: bool) -> Result<Vec<String>> {
    let data = if src == "-" {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        buf
    } else {
        std::fs::read(src)?
    };
    let sep = if nul { b'\0' } else { b'\n' };
    let mut out = Vec::new();
    for chunk in data.split(|&b| b == sep) {
        let mut c = chunk;
        if !nul && c.last() == Some(&b'\r') {
            c = &c[..c.len() - 1];
        }
        if c.is_empty() {
            continue;
        }
        out.push(c.to_str_lossy().into_owned());
    }
    Ok(out)
}

/// Return `true` if any path in `iter` equals `p` or lives under the directory
/// `p` (i.e. starts with `p` + `/`), the way a directory pathspec matches.
fn path_is_or_under<'a>(mut iter: impl Iterator<Item = &'a BString>, p: &str) -> bool {
    let pb = p.as_bytes();
    let mut prefix = pb.to_vec();
    prefix.push(b'/');
    iter.any(|x| x.as_slice() == pb || x.as_slice().starts_with(&prefix))
}
