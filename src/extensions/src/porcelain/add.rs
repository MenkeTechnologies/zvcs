//! `git add` — stage worktree paths into the index, served natively via the
//! vendored gitoxide crates so tools on PATH see the same staged index.
//!
//! Supported forms (the dominant `git add` invocations):
//! ```text
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
//!   * `git add <submodule>` — stage a moved submodule's current HEAD as the
//!     parent gitlink (mode 160000), the same way stock git does; no
//!     fast-forward gate and no commit (that is `git zbump`)
//!   * `git add <dir>` where `<dir>` is itself a git repository — records a
//!     gitlink to its current HEAD and warns (`advice.addEmbeddedRepo`)
//!   * flags `-f/--force`, `-n/--dry-run`, `-v/--verbose`, `--sparse`, `--`, and
//!     `--warn-embedded-repo`/`--no-warn-embedded-repo` (mutes that warning)
//! ```
//!
//! For each matched worktree file the blob is hashed into the object database and
//! its index entry is (re)written with the current mode and filesystem stat.
//! Tracked paths whose worktree file is gone are staged as deletions, matching
//! modern `git add` semantics. Unmerged (conflicted) entries under a matched path
//! are collapsed to the freshly-staged stage-0 entry.
//!
//! Content goes through the same filter pipeline git runs on the way into the
//! object database — `clean` drivers, `working-tree-encoding`, `ident` and the
//! EOL normalization `text`/`core.autocrlf` ask for — so the staged blob is the
//! one git would write, `core.safecrlf=true` refuses an unsafe conversion with
//! `fatal: CRLF would be replaced by LF in <path>` (exit 128, nothing staged),
//! and the round-trip warning reaches stderr. A symlink's target is stored
//! verbatim, as git stores it.
//!
//! Deviations (bailed or noted, never faked):
//! ```text
//!   * an embedded git repository is staged as a gitlink but is not registered in
//!     `.gitmodules` — the same as stock git, which only warns.
//!   * `-i`/`--interactive` runs the numbered main menu ([`super::add_interactive`])
//!     and `-p`/`--patch` the hunk selector ([`super::add_patch`]). `-e`/`--edit`
//!     (diff into an editor, then `apply --recount --cached`) is rejected.
//!   * `-U/--unified`, `--inter-hunk-context`, `--[no-]auto-advance` only configure
//!     the interactive/patch diff. Their values are validated exactly as git's
//!     `OPT_INTEGER` (bad value ⇒ exit 129) and, without `-p`/`-i`, git's
//!     `fatal: the option '<x>' requires '--interactive/--patch'` (exit 128) is
//!     reproduced. A bare `--auto-advance` is the default and stages normally;
//!     only `--no-auto-advance` triggers it.
//! ```

use anyhow::{bail, Result};
use std::collections::HashSet;
use std::io::Read;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::index::entry::{Flags, Mode, Stage, Stat};

use super::{Arg, LongOpt};

/// `cmd_add()`'s `struct option builtin_add_options[]` (builtin/add.c), in table
/// order, as [`super::resolve_long`] reads it. `-h` and the short-only bundle are
/// answered by the parse loop itself and have no entry here.
///
/// `--unified` and `--inter-hunk-context` come from `OPT_DIFF_UNIFIED` /
/// `OPT_DIFF_INTERHUNK_CONTEXT`, both `PARSE_OPT_NONEG`, so neither has a `--no-`
/// spelling; every other entry does.
pub(super) const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "dry-run",                     neg: true,  arg: Arg::None },
    LongOpt { name: "verbose",                     neg: true,  arg: Arg::None },
    LongOpt { name: "interactive",                 neg: true,  arg: Arg::None },
    LongOpt { name: "patch",                       neg: true,  arg: Arg::None },
    LongOpt { name: "auto-advance",                neg: true,  arg: Arg::None },
    LongOpt { name: "unified",                     neg: false, arg: Arg::Required },
    LongOpt { name: "inter-hunk-context",          neg: false, arg: Arg::Required },
    LongOpt { name: "edit",                        neg: true,  arg: Arg::None },
    LongOpt { name: "force",                       neg: true,  arg: Arg::None },
    LongOpt { name: "update",                      neg: true,  arg: Arg::None },
    LongOpt { name: "renormalize",                 neg: true,  arg: Arg::None },
    LongOpt { name: "intent-to-add",               neg: true,  arg: Arg::None },
    LongOpt { name: "all",                         neg: true,  arg: Arg::None },
    LongOpt { name: "ignore-removal",              neg: true,  arg: Arg::None },
    LongOpt { name: "refresh",                     neg: true,  arg: Arg::None },
    LongOpt { name: "ignore-errors",               neg: true,  arg: Arg::None },
    LongOpt { name: "ignore-missing",              neg: true,  arg: Arg::None },
    LongOpt { name: "sparse",                      neg: true,  arg: Arg::None },
    LongOpt { name: "chmod",                       neg: true,  arg: Arg::Required },
    LongOpt { name: "warn-embedded-repo",          neg: true,  arg: Arg::None },
    LongOpt { name: "pathspec-from-file",          neg: true,  arg: Arg::Required },
    LongOpt { name: "pathspec-file-nul",           neg: true,  arg: Arg::None },
];

pub fn add(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;
    if repo.workdir().is_none() {
        crate::git_fatal!("this operation must be run in a work tree");
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
    // `--sparse` (git's `include_sparse`): stage paths outside the sparse-checkout
    // definition instead of reporting them.
    let mut include_sparse = false;
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
    // `warn_on_embedded_repo` (builtin/add.c): on by default, cleared by the
    // hidden `--no-warn-embedded-repo`.
    let mut warn_embedded = true;
    // `--no-all`/`--ignore-removal`: stage adds+mods but not worktree deletions.
    let mut no_removal = false;
    // Some(true) => `--chmod=+x`, Some(false) => `--chmod=-x`.
    let mut chmod: Option<bool> = None;
    let mut from_file: Option<String> = None;
    let mut file_nul = false;
    // `-U`/`--unified`, `--inter-hunk-context` and `--[no-]auto-advance` configure
    // the interactive hunk selector; shared verbatim with `git reset`/`git checkout`.
    let mut patch_opts = super::reset::PatchDiffOpts::default();
    // `-p`/`--patch` and `-i`/`--interactive` (git's `patch_interactive` and
    // `add_interactive`; both are `OPT_BOOL`, so the `--no-` forms clear them).
    let mut patch_interactive = false;
    let mut add_interactive = false;
    let mut pathspecs: Vec<String> = Vec::new();
    let mut positional_only = false;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        // A value still owed to `-U`/`--unified`/`--inter-hunk-context` is taken
        // verbatim, even past `--`, the way parse-options takes it — and precisely
        // because it is a value, it is never resolved as an option name.
        if patch_opts.awaiting_value() {
            match patch_opts.take_arg(a) {
                Err(code) => return Ok(code),
                Ok(true) => {
                    i += 1;
                    continue;
                }
                Ok(false) => {}
            }
        }
        if positional_only {
            pathspecs.push(a.clone());
            i += 1;
            continue;
        }
        // Respell a unique abbreviation as the name it resolves to, ahead of both
        // the shared value-option handler and the match below, so `--unif 3` and
        // `--intent-to` land where their full spellings land.
        let canonical;
        let a = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };
        match patch_opts.take_arg(a) {
            Err(code) => return Ok(code),
            Ok(true) => {
                i += 1;
                continue;
            }
            Ok(false) => {}
        }
        match a {
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
            "--no-refresh" => refresh = false,
            // `--renormalize` re-stages tracked paths (implies -u). Content filters
            // are not applied here, so it restages the verbatim worktree bytes.
            "--renormalize" => renormalize = true,
            "--no-renormalize" => renormalize = false,
            // `--sparse` lets the add reach paths the sparse-checkout definition
            // leaves out of the worktree; without it those paths are reported and
            // skipped (`advise_on_updating_sparse_paths()`).
            "--sparse" => include_sparse = true,
            "--no-sparse" => include_sparse = false,
            // `--warn-embedded-repo`/`--no-warn-embedded-repo` (hidden in git,
            // an `OPT_HIDDEN_BOOL` over `warn_on_embedded_repo`, default on) mutes
            // the `adding embedded git repository:` warning and its advice; the
            // gitlink is staged either way.
            "--warn-embedded-repo" => warn_embedded = true,
            "--no-warn-embedded-repo" => warn_embedded = false,
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
            // `chmod` is an `OPT_STRING`, whose unset writes NULL over whatever an
            // earlier `--chmod=<v>` recorded (parse-options.c:200-202) — including
            // the validation that value would have failed, since `cmd_add()` only
            // inspects the surviving string.
            "--no-chmod" => chmod = None,
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
            // `OPT_FILENAME`'s unset writes NULL (parse-options.c:214-215), so a
            // later `--no-pathspec-from-file` discards an earlier value and the
            // pathspecs come from argv again.
            "--no-pathspec-from-file" => from_file = None,
            // Interactive hunk selection (`add-patch.c`), served by
            // [`super::add_patch`]. `-e`/`--edit` (diff the worktree into an
            // editor and `apply --recount --cached` the result) is a separate
            // machine and stays unported.
            "-p" | "--patch" => patch_interactive = true,
            "--no-patch" => patch_interactive = false,
            "-i" | "--interactive" => add_interactive = true,
            "--no-interactive" => add_interactive = false,
            "-e" | "--edit" => bail!("edit mode (-e/--edit) needs an interactive editor; not ported"),
            // `edit_interactive` is an `OPT_BOOL`, so its unset writes 0 — which is
            // the state this command already starts in. Nothing to refuse: git runs
            // the ordinary add for `--no-edit`, and so does this.
            "--no-edit" => {}
            // `-h` is handled by `parse_options()` before any other switch in the
            // same bundle, so `git add -hv` still prints the table.
            other if other.starts_with('-')
                && !other.starts_with("--")
                && other[1..].contains('h') =>
            {
                return print_usage();
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
                        'p' => patch_interactive = true,
                        'i' => add_interactive = true,
                        _ => return usage_error(format!("unknown switch `{c}'")),
                    }
                }
            }
            other if other.starts_with('-') => return usage_error(format!("unknown option `{}'", other.trim_start_matches('-'))),
            // A non-option argument is handed back unchanged by the resolver.
            _ => pathspecs.push(a.to_string()),
        }
        i += 1;
    }

    if let Err(code) = patch_opts.finish() {
        return Ok(code);
    }

    // git's `cmd_add` order: the two `cannot be negative` fatals, then `-p`
    // implying `-i`, then either the interactive hand-off (with its two
    // "cannot be used together" fatals) or the three
    // `requires '--interactive/--patch'` fatals — all of it before pathspec
    // setup, `--ignore-missing`, the empty-pathspec check and the `-A`/`-u`
    // conflict (verified against git 2.55.0).
    if patch_interactive {
        add_interactive = true;
    }
    if let Some(code) = patch_opts.require_patch_named(add_interactive, "--interactive/--patch") {
        return Ok(code);
    }
    if add_interactive {
        if dry_run {
            return usage_fatal(
                "options '--dry-run' and '--interactive/--patch' cannot be used together".into(),
            );
        }
        if from_file.is_some() {
            return usage_fatal(
                "options '--pathspec-from-file' and '--interactive/--patch' cannot be used together"
                    .into(),
            );
        }
        if !patch_interactive {
            // `git add -i`'s numbered main menu ([`super::add_interactive`]).
            return Ok(ExitCode::from(super::add_interactive::run_status(
                &repo,
                patch_opts.to_interactive(false),
                &pathspecs,
            )?));
        }
        return super::add_patch::run(
            &repo,
            super::add_patch::Mode::Add,
            None,
            patch_opts.to_interactive(false),
            &pathspecs,
        );
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
        .is_none_or(|p| p.as_os_str().is_empty());
    // `prefix_path()` runs every command-line path through `normalize_path_copy()`
    // first, so `./.` is `.`, `src/.` is `src`, and `a/../b` is `b` before anything
    // asks whether the path exists or is ignored.
    for spec in pathspecs.iter_mut() {
        *spec = normalize_pathspec(spec);
    }
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
    // The content filters git runs on the way into the object database:
    // `.gitattributes` `clean` drivers, `working-tree-encoding`, `ident`, and the
    // EOL normalization `text`/`core.autocrlf` ask for. `git add` hashes the
    // *converted* bytes, so staging the verbatim worktree copy writes a different
    // blob than git does in any repository that normalizes line endings.
    let mut filters = super::convert_to_git::WorktreeFilter::new(&repo, write_content, renormalize)?;
    // `path_in_sparse_checkout()`: without `--sparse`, a path the sparse-checkout
    // definition leaves out of the worktree is skipped and reported instead of
    // staged. Loaded only when there is a definition to consult.
    let sparsity = if !include_sparse
        && repo
            .config_snapshot()
            .boolean("core.sparseCheckout")
            .unwrap_or(false)
    {
        Some(super::sparse_checkout::load_sparsity(&repo)?)
    } else {
        None
    };
    let outside_sparse = |path: &BString| -> bool {
        sparsity.as_ref().is_some_and(|s| !s.includes(&path.to_str_lossy()))
    };
    // `matched_sparse_paths`: what the message at the end names, sorted and unique.
    let mut sparse_skipped: std::collections::BTreeSet<BString> = Default::default();
    // Paths that could not be read, paired with the OS error text git reports
    // (only surfaced for a real add). git prints `open("<p>"): <strerror>`.
    let mut read_errors: Vec<(BString, String)> = Vec::new();
    // Embedded repositories whose HEAD is unborn: git cannot record a gitlink for
    // them and reports each one before failing the whole add.
    let mut headless_repos: Vec<BString> = Vec::new();
    // `check_embedded_repo`'s `adviced_on_embedded_repo`: the warning is printed
    // per repository, the advice at most once per invocation.
    let mut embedded_advised = false;

    // git stages in two passes: `update_files_in_cache()` walks the *index* (so the
    // tracked matches come first, in path order), then `add_files()` walks the sorted
    // `dir->entries` for the new ones. Everything the staging emits — the `-v`/`-n`
    // report, the `core.safecrlf` warnings, the read errors — comes out in that order,
    // so the walk results are put in it before any file is touched.
    let mut walked: Vec<gix::dir::Entry> = Vec::new();
    for item in iter.by_ref() {
        walked.push(item?.entry);
    }
    walked.sort_by(|a, b| {
        let (a_new, b_new) = (
            !existing.contains(&a.rela_path),
            !existing.contains(&b.rela_path),
        );
        a_new.cmp(&b_new).then_with(|| a.rela_path.cmp(&b.rela_path))
    });

    for entry in walked {
        let path = entry.rela_path;
        let already_tracked = existing.contains(&path);
        // Ignore semantics: an ignored path is only staged if forced or already
        // tracked. Tracked/untracked (non-ignored) paths are always eligible.
        let ignored_here =
            matches!(entry.status, gix::dir::entry::Status::Ignored(_)) && !force && !already_tracked;

        // Only regular files and symlinks are stageable *content* here; skip
        // directories and anything untrackable. A directory that is itself a git
        // repository is staged as a gitlink instead — an untracked one right here
        // (git's `check_embedded_repo` path), a tracked one in the index-driven
        // pass after this loop.
        match entry.disk_kind {
            Some(gix::dir::entry::Kind::File) | Some(gix::dir::entry::Kind::Symlink) => {}
            Some(gix::dir::entry::Kind::Repository) => {
                if already_tracked || ignored_here || tracked_only {
                    continue;
                }
                let Some(abs) = repo.workdir_path(&path) else { continue };
                // `add_file_to_index` resolves the embedded repository's HEAD into
                // the gitlink; an unborn HEAD has nothing to record and is the
                // `does not have a commit checked out` failure.
                let Some(head) = gix::open(&abs)
                    .ok()
                    .and_then(|sub| sub.head_id().ok().map(|h| h.detach()))
                else {
                    headless_repos.push(path);
                    continue;
                };
                warn_embedded_repo(&path, warn_embedded, &repo, &mut embedded_advised);
                let stat = gix::index::fs::Metadata::from_path_no_follow(&abs)
                    .ok()
                    .and_then(|md| Stat::from_fs(&md).ok())
                    .unwrap_or_default();
                staged.push(Staged {
                    path,
                    id: head,
                    mode: Mode::COMMIT,
                    stat,
                    was_tracked: false,
                });
                continue;
            }
            _ => continue,
        }

        if ignored_here {
            continue;
        }
        // `-u/--update`, `--refresh`, `--renormalize` restage tracked paths only.
        if tracked_only && !already_tracked {
            continue;
        }
        // `add_files()`: a path this add would otherwise stage but that lies outside
        // the sparse-checkout definition is collected for the report instead. The
        // check sits after the eligibility filters, so a path `-u` was never going to
        // stage is not reported either.
        if outside_sparse(&path) {
            sparse_skipped.insert(path);
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
            // A symlink's target is stored verbatim; a regular file goes through
            // the pipeline, which is also where git's CRLF round-trip warning
            // (and `core.safecrlf`'s refusal) comes from.
            let bytes = {
                let rela = gix::path::from_bstr(path.as_bstr()).into_owned();
                match filters.convert(&repo, &rela, &bytes) {
                    Ok(converted) => converted,
                    Err(err) => {
                        // `core.safecrlf=true` makes an unsafe conversion fatal:
                        // git names the path, stages nothing and exits 128.
                        eprintln!("fatal: {err}");
                        return Ok(ExitCode::from(128));
                    }
                }
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

    // --- submodule gitlinks: stage a moved submodule's new HEAD (mode 160000) ---
    // `git add <submodule>` records the submodule worktree's current HEAD as the
    // parent's gitlink. The worktree walk above yields only blobs and symlinks (a
    // submodule dir is `Kind::Repository`, dropped there), so tracked gitlinks are
    // staged from the index here — driven by the index rather than the walk, so it
    // does not depend on how the walk treats a repository directory. These are
    // plain `git add` semantics: whatever HEAD the submodule sits at, with NO
    // fast-forward gate and NO commit (that is `git zbump`). An unchanged gitlink
    // stages nothing, matching git. `--refresh` returns before the write below, so
    // it still only refreshes stat and never records a pointer move.
    for (path, id, stat) in moved_gitlinks(&repo, &index, &staged_set, |p| {
        pathspec.is_included(p, Some(false))
    }) {
        staged.push(Staged { path, id, mode: Mode::COMMIT, stat, was_tracked: true });
    }
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
            // `PS_IGNORE_SKIP_WORKTREE`: an entry the sparse-checkout definition keeps
            // out of the worktree is absent by design, never a deletion.
            if e.flags.contains(gix::index::entry::Flags::SKIP_WORKTREE) {
                continue;
            }
            let path = e.path_in(backing);
            let owned = path.to_owned();
            if staged_set.contains(&owned) {
                continue;
            }
            if outside_sparse(&owned) {
                sparse_skipped.insert(owned);
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
        // An embedded repository whose HEAD is unborn matched the pathspec — it
        // just could not be indexed — so it is not a "did not match" case.
        let matched_headless = path_is_or_under(headless_repos.iter(), p);
        // Neither is a path held back by the sparse-checkout definition: it matched,
        // and the sparse report at the end is what git says about it.
        let matched_sparse = path_is_or_under(sparse_skipped.iter(), p);

        if matched_staged || matched_tracked || matched_deleted || matched_headless || matched_sparse
        {
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

    // `--ignore-errors`: a real add reports the paths it could not index and, if
    // any occurred without `--ignore-errors`, aborts before touching the index.
    // An embedded repository with an unborn HEAD is one of those paths; git names
    // it with the trailing slash the directory walk carries.
    if !(read_errors.is_empty() && headless_repos.is_empty()) && !dry_run {
        for p in &headless_repos {
            eprintln!("error: '{p}/' does not have a commit checked out");
            eprintln!("error: unable to index file '{p}/'");
        }
        for (p, msg) in &read_errors {
            eprintln!("error: open(\"{p}\"): {msg}");
            eprintln!("error: unable to index file '{p}'");
        }
        if !ignore_errors {
            eprintln!("fatal: adding files failed");
            return Ok(ExitCode::from(128));
        }
    }

    // `-N` reaches `set_object_name_for_intent_to_add_entry()` for every path
    // `add_files_to_cache()` and `add_files()` actually index, and that helper
    // writes the empty blob before `add_to_index()` ever looks at
    // `ADD_CACHE_PRETEND` — so `--dry-run` leaves the object behind too. A
    // pathspec that matched only unchanged tracked paths indexes nothing and
    // therefore writes nothing.
    let intent_visited = intent_to_add && {
        let changed: std::collections::HashMap<&BString, &Staged> =
            staged.iter().filter(|s| s.was_tracked).map(|s| (&s.path, s)).collect();
        let backing = index.path_backing();
        staged.iter().any(|s| !s.was_tracked)
            || index.entries().iter().any(|e| {
                e.stage() == Stage::Unconflicted
                    && e.mode != Mode::COMMIT
                    && changed
                        .get(&e.path_in(backing).to_owned())
                        // `--renormalize` indexes every matched blob, changed or
                        // not, so any match at all reaches `add_to_index()`.
                        .is_some_and(|s| renormalize || s.id != e.id || s.mode != e.mode)
            })
    };
    if intent_visited {
        repo.write_blob(b"")?;
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
                // A tracked path is reported only when the worktree really
                // differs from its index entry: `add_files_to_cache()` drives
                // the report from `run_diff_files()`, which never hands an
                // unchanged path to `add_file_to_index()` at all. `-N` does not
                // widen that — it changes what gets *staged* for a path already
                // known to differ, not which paths are visited.
                //
                // `--renormalize` is the exception, and reports every matched
                // blob: `renormalize_tracked_files()` walks the index rather
                // than a diff, and `add_to_index()` skips its `alias` lookup
                // under `ADD_CACHE_RENORMALIZE` — so `was_same` is never true
                // and the `add '<path>'` line is unconditional.
                if renormalize || s.id != e.id || s.mode != e.mode {
                    lines.push(format!("add '{path}'"));
                }
            }
        }
        // `read_directory()` sorts `dir->entries` before `add_files()` walks them, so
        // the new paths are reported in path order rather than in the order the
        // directory walk happened to reach them.
        let mut fresh: Vec<&BString> = staged.iter().filter(|s| !s.was_tracked).map(|s| &s.path).collect();
        fresh.sort();
        for path in fresh {
            lines.push(format!("add '{path}'"));
        }
        lines
    };

    if staged.is_empty() && deletions.is_empty() {
        return Ok(finish_code(
            !read_errors.is_empty() || !headless_repos.is_empty(),
            ignore_errors,
            dry_run,
            &sparse_skipped,
        ));
    }

    // --- dry run: report only, never touch the index ------------------------
    if dry_run {
        for line in &report {
            println!("{line}");
        }
        return Ok(finish_code(
            !read_errors.is_empty() || !headless_repos.is_empty(),
            ignore_errors,
            dry_run,
            &sparse_skipped,
        ));
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
        record_stage_event(&repo, staged.len() + deletions.len());

        if verbose {
            for line in &report {
                println!("{line}");
            }
        }
        return Ok(finish_code(
            !read_errors.is_empty() || !headless_repos.is_empty(),
            ignore_errors,
            dry_run,
            &sparse_skipped,
        ));
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
    record_stage_event(&repo, staged.len() + deletions.len());

    if verbose {
        for line in &report {
            println!("{line}");
        }
    }

    Ok(finish_code(
        !read_errors.is_empty() || !headless_repos.is_empty(),
        ignore_errors,
        dry_run,
        &sparse_skipped,
    ))
}

/// Every stage-0 gitlink the pathspec selects whose submodule worktree sits at a
/// commit other than the one recorded, as `(path, submodule HEAD, stat)`.
///
/// `git add <submodule>` records the submodule worktree's current HEAD as the
/// parent gitlink (mode 160000). A worktree walk cannot serve this — a submodule
/// directory comes out of it as `Kind::Repository`, which carries no stageable
/// content — so this is driven off the index instead, which also keeps it
/// independent of how any given walk treats a repository directory.
///
/// An unchanged gitlink yields nothing, exactly as git stages nothing for one, and
/// a submodule whose worktree has no resolvable HEAD (uninitialized, or unborn) is
/// left alone rather than guessed at. `skip` is the set of paths the caller has
/// already staged.
///
/// Shared with the `stage` verb, which stock git implements as the very same
/// `cmd_add` and which must therefore record the same pointer moves.
pub(crate) fn moved_gitlinks(
    repo: &gix::Repository,
    index: &gix::index::File,
    skip: &HashSet<BString>,
    mut selected: impl FnMut(&gix::bstr::BStr) -> bool,
) -> Vec<(BString, gix::ObjectId, Stat)> {
    let backing = index.path_backing();
    let mut out = Vec::new();
    for e in index.entries() {
        if e.stage() != Stage::Unconflicted || e.mode != Mode::COMMIT {
            continue; // only stage-0 gitlinks
        }
        let path = e.path_in(backing);
        if !selected(path) || skip.contains(&path.to_owned()) {
            continue;
        }
        // Resolve the submodule worktree's current HEAD commit. gix::open reads
        // its `.git` gitfile, so this works whether or not the gitlink is also
        // declared in .gitmodules (git add does not require it to be).
        let Some(abs) = repo.workdir_path(path) else { continue };
        let Some(new_id) = gix::open(&abs)
            .ok()
            .and_then(|sr| sr.head_id().ok().map(|h| h.detach()))
        else {
            continue; // uninitialized or unborn HEAD → nothing to stage
        };
        if new_id == e.id {
            continue; // gitlink already at HEAD → git stages nothing
        }
        let stat = gix::index::fs::Metadata::from_path_no_follow(&abs)
            .ok()
            .and_then(|md| Stat::from_fs(&md).ok())
            .unwrap_or_default();
        out.push((path.to_owned(), new_id, stat));
    }
    out
}

/// Record a `stage` event in the live feed (`git zevents`/`ztail`) after a
/// successful index write, so `git add` shows in the tree-wide activity feed
/// alongside commits, reconciles, and status changes. Best-effort: no daemon/db
/// just means no feed entry, never a failed add. Shared with the `stage` verb.
pub(crate) fn record_stage_event(repo: &gix::Repository, count: usize) {
    if count == 0 {
        return;
    }
    let Some(workdir) = repo.workdir() else { return };
    let git_dir = repo.git_dir().canonicalize().unwrap_or_else(|_| repo.git_dir().to_path_buf());
    let workdir = workdir.canonicalize().unwrap_or_else(|_| workdir.to_path_buf());
    if let Ok(conn) = crate::db::open_rw() {
        if let Ok(repo_id) = crate::db::upsert_repo(&conn, &git_dir, Some(&workdir)) {
            let detail = format!("staged {count} path(s)");
            let _ = crate::db::record_event(&conn, "stage", Some(repo_id), Some(&detail), None, None);
        }
    }
}

/// `normalize_path_copy_len()` (path.c), the pass `prefix_path()` puts every
/// command-line path through: a `.` component disappears, a `..` component pops the
/// one before it, and repeated slashes collapse into one. So `./.` reaches the
/// pathspec machinery as `.`, `src/.` as `src`, and `a/../b` as `b`.
///
/// A pathspec that carries magic (`:(icase)x`, `:/`, …) is left alone: git parses the
/// magic first and normalizes only the path that follows, and the magic forms this
/// command sees are already repo-relative.
fn normalize_pathspec(spec: &str) -> String {
    if spec.starts_with(':') || spec.is_empty() {
        return spec.to_string();
    }
    let absolute = spec.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for component in spec.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                // A leading `..` has nothing to pop and stays, as git keeps it for the
                // "outside repository" diagnostics further on.
                if matches!(out.last(), Some(&last) if last != "..") {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    let joined = out.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        // Everything cancelled out: the argument named the directory it was run in.
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

/// The overall exit code: git returns 1 from a real add when `--ignore-errors`
/// let it skip at least one unreadable file, and 1 whenever a matched path lay
/// outside the sparse-checkout definition; else success.
///
/// `advise_on_updating_sparse_paths()` names every skipped path — sorted, one per
/// line, under a three-line explanation — and follows with the advice block that
/// `advice.updateSparsePath` turns off.
fn finish_code(
    had_errors: bool,
    ignore_errors: bool,
    dry_run: bool,
    sparse_skipped: &std::collections::BTreeSet<BString>,
) -> ExitCode {
    if !sparse_skipped.is_empty() {
        eprintln!("The following paths and/or pathspecs matched paths that exist");
        eprintln!("outside of your sparse-checkout definition, so will not be");
        eprintln!("updated in the index:");
        for path in sparse_skipped {
            eprintln!("{path}");
        }
        if crate::advice::enabled("updateSparsePath") {
            eprintln!("hint: If you intend to update such entries, try one of the following:");
            eprintln!("hint: * Use the --sparse option.");
            eprintln!("hint: * Disable or modify the sparsity rules.");
            eprintln!(
                "hint: Disable this message with \"git config set advice.updateSparsePath false\""
            );
        }
        return ExitCode::from(1);
    }
    if ignore_errors && !dry_run && had_errors {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Port of `check_embedded_repo()` (builtin/add.c): staging a directory that is
/// itself a git repository records a gitlink, so the outer repository ends up
/// pointing at a commit nobody can fetch. git warns about every such path and,
/// through `advise_if_enabled(ADVICE_ADD_EMBEDDED_REPO, …)`, explains the two
/// ways out at most once per invocation — which is what `advised` tracks.
///
/// `--no-warn-embedded-repo` clears `warn`, silencing both.
fn warn_embedded_repo(
    path: &BString,
    warn: bool,
    repo: &gix::Repository,
    advised: &mut bool,
) {
    if !warn {
        return;
    }
    eprintln!("warning: adding embedded git repository: {path}");
    if *advised {
        return;
    }
    *advised = true;
    crate::advice::Advice::AddEmbeddedRepo.advise_in(
        repo,
        &format!(
            "You've added another git repository inside your current repository.\n\
             Clones of the outer repository will not contain the contents of\n\
             the embedded repository and will not know how to obtain it.\n\
             If you meant to add a submodule, use:\n\
             \n\
             \tgit submodule add <url> {path}\n\
             \n\
             If you added this path by mistake, you can remove it from the\n\
             index with:\n\
             \n\
             \tgit rm --cached {path}\n\
             \n\
             See \"git help submodule\" for more information."
        ),
    );
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

/// `--chmod` value parse: `+x` => `Some(true)`, `-x` => `Some(false)`, else `None`.
fn parse_chmod(v: &str) -> Option<bool> {
    match v {
        "+x" => Some(true),
        "-x" => Some(false),
        _ => None,
    }
}

/// `usage_with_options()` rendering of `builtin/add.c`'s option table, verbatim —
/// including the blank line after the synopsis, the group break before `-i`, the
/// continuation lines for names too long for the description column, and the
/// trailing blank line.
const USAGE: &str = concat!(
    "usage: git add [<options>] [--] <pathspec>...\n",
    "\n",
    "    -n, --[no-]dry-run    dry run\n",
    "    -v, --[no-]verbose    be verbose\n",
    "\n",
    "    -i, --[no-]interactive\n",
    "                          interactive picking\n",
    "    -p, --[no-]patch      select hunks interactively\n",
    "    --[no-]auto-advance   auto advance to the next file when selecting hunks interactively\n",
    "    -U, --unified <n>     generate diffs with <n> lines context\n",
    "    --inter-hunk-context <n>\n",
    "                          show context between diff hunks up to the specified number of lines\n",
    "    -e, --[no-]edit       edit current diff and apply\n",
    "    -f, --[no-]force      allow adding otherwise ignored files\n",
    "    -u, --[no-]update     update tracked files\n",
    "    --[no-]renormalize    renormalize EOL of tracked files (implies -u)\n",
    "    -N, --[no-]intent-to-add\n",
    "                          record only the fact that the path will be added later\n",
    "    -A, --[no-]all        add changes from all tracked and untracked files\n",
    "    --[no-]ignore-removal ignore paths removed in the working tree (same as --no-all)\n",
    "    --[no-]refresh        don't add, only refresh the index\n",
    "    --[no-]ignore-errors  just skip files which cannot be added because of errors\n",
    "    --[no-]ignore-missing check if - even missing - files are ignored in dry run\n",
    "    --[no-]sparse         allow updating entries outside of the sparse-checkout cone\n",
    "    --[no-]chmod (+|-)x   override the executable bit of the listed files\n",
    "    --[no-]pathspec-from-file <file>\n",
    "                          read pathspec from file\n",
    "    --[no-]pathspec-file-nul\n",
    "                          with --pathspec-from-file, pathspec elements are separated with NUL character\n",
    "\n",
);

/// `-h`: `parse_options()` prints the whole table on *stdout* and still exits 129.
fn print_usage() -> Result<ExitCode> {
    print!("{USAGE}");
    Ok(ExitCode::from(129))
}

/// A usage error (git exit 129): unknown option/switch. git names the offending
/// option, then prints the same table `-h` does — on stderr this time.
fn usage_error(msg: String) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    eprint!("{USAGE}");
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
///
/// Index paths are normalized — no `./`, no trailing `/` — while a pathspec the
/// user typed may carry either, so `p` is normalized the same way before
/// comparing. Without that, `git add d/` matched nothing and was reported as a
/// gitignored path.
fn path_is_or_under<'a>(mut iter: impl Iterator<Item = &'a BString>, p: &str) -> bool {
    let mut p = p;
    while let Some(rest) = p.strip_prefix("./") {
        p = rest;
    }
    let pb = p.trim_end_matches('/').as_bytes();
    let mut prefix = pb.to_vec();
    prefix.push(b'/');
    iter.any(|x| x.as_slice() == pb || x.as_slice().starts_with(&prefix))
}
