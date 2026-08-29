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
//!   * `git add --chmod=(+|-)x` — override the executable bit of every matched
//!     index entry, staged by this run or not ([`chmod_pathspec`])
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
use std::collections::{HashMap, HashSet};
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
    let repo = crate::setup::discover()?;
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
    // The raw value of the last `--chmod=<v>`. `OPT_STRING` only *records* it
    // during parsing; `cmd_add()` validates the survivor further down
    // (builtin/add.c:447-449), which is why this is a string and not a bool.
    let mut chmod_arg: Option<String> = None;
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
        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122): tested on the token as typed, ahead of the
        // abbreviation resolver, because it is a `strcmp` — `--help-a` and
        // `--help-all=x` stay unknown options. It renders `USAGE_FULL`, which
        // for `add` is `USAGE` plus the hidden `--warn-embedded-repo`.
        if a == "--help-all" {
            return print_usage_all();
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
            // The value is only recorded here — `OPT_STRING` does no validation —
            // so a bad one cannot outrank the fatals `cmd_add()` raises first.
            // `unwrap_or_default()` turned a missing value into an empty one, so
            // `git add --chmod` reported `fatal: --chmod param ''` at 128 where
            // stock never gets that far: `get_arg()` refuses first with
            // ``error: option `chmod' requires a value`` at 129.
            "--chmod" => {
                i += 1;
                chmod_arg = Some(super::value_at(args, i, a)?.to_string());
            }
            // `chmod` is an `OPT_STRING`, whose unset writes NULL over whatever an
            // earlier `--chmod=<v>` recorded (parse-options.c:200-202) — including
            // the validation that value would have failed, since `cmd_add()` only
            // inspects the surviving string.
            "--no-chmod" => chmod_arg = None,
            s if s.starts_with("--chmod=") => {
                chmod_arg = Some(s["--chmod=".len()..].to_string());
            }
            "--pathspec-from-file" => {
                i += 1;
                from_file = Some(super::value_at(args, i, a)?.to_string());
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
            other if other.starts_with('-') => return Ok(super::unknown_option(other, USAGE)),
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

    // `if (addremove && take_worktree_changes) die(...)` (builtin/add.c): `-A` stages
    // untracked files and `-u` refuses to, so asking for both is a fatal. `addremove`
    // is the *explicit* `-A`/`--no-all` setting — bare `-u` turns git's default off
    // instead of tripping this — which is exactly what `all` records here, last
    // occurrence winning (`-A --no-all -u` is accepted, `-u --no-all -A` is not).
    //
    // Its position is load-bearing: verified against git 2.55.0, this fatal is the
    // only output of `--chmod=bogus -A -u`, `-A -u --ignore-missing`,
    // `--pathspec-from-file=/nope -A -u`, `-A -u --pathspec-file-nul` and `-A -u ''`,
    // so it outranks every check below — while `-n -p -A -u` and `-U 3 -A -u` still
    // report the interactive-mode fatals above, which outrank it.
    if all && update_only {
        return usage_fatal("options '-A' and '-u' cannot be used together".into());
    }

    // `--ignore-missing` is only meaningful with `--dry-run` (builtin/add.c:444).
    if ignore_missing && !dry_run {
        return usage_fatal("the option '--ignore-missing' requires '--dry-run'".into());
    }

    // `if (chmod_arg && (...)) die(...)` (builtin/add.c:447-449): the value is
    // validated HERE, once, on whatever survived the parse — after the `-A`/`-u`
    // and `--ignore-missing` fatals, and before `parse_pathspec()` and every check
    // built on it. Verified against git 2.55.0: `--chmod=bogus -A -u` reports the
    // `-A`/`-u` fatal and `--chmod=bogus --ignore-missing` the `--dry-run` one,
    // while `--chmod=bogus --pathspec-from-file=/nope`, `--chmod=bogus ''` and
    // `--chmod=bogus --pathspec-file-nul` all report the chmod fatal. Validating
    // during the parse instead (as this did) inverted the first two.
    let chmod: Option<bool> = match &chmod_arg {
        None => None,
        Some(v) => match parse_chmod(v) {
            Some(b) => Some(b),
            None => return usage_fatal(format!("--chmod param '{v}' must be either -x or +x")),
        },
    };

    // git rejects an empty-string pathspec outright — inside `parse_pathspec()`,
    // so it fires before `--pathspec-from-file` is even opened.
    if pathspecs.iter().any(String::is_empty) {
        return usage_fatal(
            "empty string is not a valid pathspec. please use . instead if you meant to match all paths"
                .into(),
        );
    }

    // `--pathspec-from-file`: read pathspecs from a file (or stdin for `-`).
    if let Some(src) = from_file {
        if !pathspecs.is_empty() {
            return usage_fatal(
                "'--pathspec-from-file' and pathspec arguments cannot be used together".into(),
            );
        }
        pathspecs = super::commit::read_pathspec_file(&src, file_nul)?;
        // The file's own elements go through the same `parse_pathspec()` the argv
        // ones did, so an empty line is the same fatal.
        if pathspecs.iter().any(String::is_empty) {
            return usage_fatal(
                "empty string is not a valid pathspec. please use . instead if you meant to match all paths"
                    .into(),
            );
        }
    } else if file_nul {
        return usage_fatal(
            "the option '--pathspec-file-nul' requires '--pathspec-from-file'".into(),
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
    //
    // `--renormalize` is excluded too, because under it `cmd_add()` calls
    // `renormalize_tracked_files()` *instead of* `add_files_to_cache()` — the walk
    // below stages nothing in git, and every object comes from the index scan, in
    // index order, stopping dead at the entry whose worktree file is gone. Writing
    // them here (walk order, all of them, before the scan can abort) left objects
    // in the store that git never wrote: `add --renormalize --all` over a
    // cone-sparse repo with `in/f.txt` deleted deposited `root.txt`'s normalized
    // blob even though git dies at `in/f.txt` first. The ids are unaffected — the
    // hash of the converted bytes is the same whether or not it is stored — so the
    // walk still computes them for the report and the index write.
    let write_content = !dry_run && !refresh && !intent_to_add && !renormalize;

    // --- index snapshot: read-only, drives staging decisions and deletions.
    // The authoritative mutation index is re-read under the lock further below.
    let index = if repo.index_path().exists() {
        repo.open_index()?
    } else {
        gix::index::File::from_state(gix::index::State::new(repo.object_hash()), repo.index_path())
    };

    // Repo-relative paths of the current stage-0 entries (tracked set), each with
    // the recorded stat and the object it names. The stat is what
    // `run_diff_files()` compares to decide which tracked paths are handed to
    // `add_file_to_index()` at all; the id and mode are `add_to_index()`'s
    // `was_same` test.
    let tracked: HashMap<BString, (Stat, gix::hash::ObjectId, Mode)> = {
        let backing = index.path_backing();
        index
            .entries()
            .iter()
            .filter(|e| e.stage() == Stage::Unconflicted)
            .map(|e| (e.path_in(backing).to_owned(), (e.stat, e.id, e.mode)))
            .collect()
    };
    // An unmerged path has no stage-0 entry, so it is not in `tracked` — but it is
    // very much *in the index*, and `-u` restages exactly the paths that are.
    // `run_diff_files()` reports an unmerged entry as modified (`diff_unmerge()`),
    // and `add_file_to_index()` then drops its three stages and writes the
    // worktree content at stage 0, which is how `git add -u` resolves a conflict.
    let existing: HashSet<BString> = {
        let backing = index.path_backing();
        tracked
            .keys()
            .cloned()
            .chain(
                index
                    .entries()
                    .iter()
                    .filter(|e| e.stage() != Stage::Unconflicted)
                    .map(|e| e.path_in(backing).to_owned()),
            )
            .collect()
    };
    // The stage-0 paths `is_racy_stat()` calls racily clean. `run_diff_files()` is
    // asked with `CE_MATCH_RACY_IS_DIRTY` and so is `add_to_index()`
    // (read-cache.c:717), so each of them is indexed — and hashed with
    // `INDEX_WRITE_OBJECT` — regardless of what its content says. That is what
    // makes `git add -N -v .` report every tracked path in a freshly built
    // worktree. See [`super::stage::racy_paths`].
    let racily_clean: HashSet<BString> = super::stage::racy_paths(&index, &repo);
    // `ce_match_stat_basic()`'s configurable field selection; see [`super::stage::stat_match`].
    let stat_match = super::stage::stat_match(&repo);

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
    // `pathspec.items[i].original` and `.match`, both 1:1 with `pathspecs`.
    let (typed, resolved): (Vec<String>, Vec<String>) =
        resolve_pathspecs(&repo, &mut pathspecs)?.into_iter().unzip();
    // `die_in_unpopulated_submodule()` and `die_path_inside_submodule()`
    // (builtin/add.c:498-499) both run before `fill_directory()`.
    if let Some(code) = die_path_inside_submodule(&index, &typed, &resolved) {
        return Ok(code);
    }
    // `fill_directory()` (builtin/add.c:510) runs before the pathspec check can die,
    // so its `could not open directory` warning is the first thing on stderr. Only
    // the modes that look for untracked files reach it (`add_new_files`,
    // builtin/add.c:451).
    if !tracked_only {
        warn_unopenable_walk_prefix(&repo, &typed, &resolved);
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
    // ```c
    // /* Set up the default git porcelain excludes */
    // if (!ignored_too) {
    //         dir.flags |= DIR_COLLECT_IGNORED;
    //         setup_standard_excludes(&dir);
    // }
    // ```
    //
    // (`builtin/add.c:504-508`.) Under `-f` git sets up no excludes at all, so the
    // walk has nothing to stop at: an ignored *directory* is descended into like
    // any other. Without that, `add -f -A` misses every file whose whole
    // directory the ignore rules covered — `build/output.o` under a `build/` rule,
    // and a `logs/` whose entries all matched `*.log` — while still picking up the
    // ignored files that sit beside tracked ones.
    let options = repo
        .dirwalk_options()?
        .emit_tracked(true)
        .emit_ignored(Some(gix::dir::walk::EmissionMode::Matching))
        .recurse_ignored_directories(force);

    let dirwalk_index = repo.index_or_load_from_head_or_empty()?;
    let mut iter = repo.dirwalk_iter(dirwalk_index, patterns.clone(), Default::default(), options)?;

    // A staged entry to be written into the index.
    struct Staged {
        path: BString,
        id: gix::hash::ObjectId,
        mode: Mode,
        stat: Stat,
        was_tracked: bool,
    }
    let mut staged: Vec<Staged> = Vec::new();
    /// Deposit the blob of every staged path — git's `index_path()` writes, all of
    /// which happen inside the odb transaction.
    ///
    /// The bytes are re-read and re-converted rather than carried out of the walk,
    /// so an `add -A` over a large worktree holds one file in memory at a time. The
    /// conversion is silent this time round (see `write_filters`); the id the walk
    /// computed is replaced by the one the write returned, so the two can never
    /// disagree even if the file changed in between.
    fn deposit_staged_blobs(
        repo: &gix::Repository,
        staged: &mut [Staged],
        filters: &mut super::convert_to_git::WorktreeFilter,
    ) -> Result<()> {
        for s in staged {
            // A gitlink has no blob and no worktree bytes: its id is the submodule's
            // HEAD, which lives in the submodule's own object database.
            if s.mode == Mode::COMMIT {
                continue;
            }
            let abs = repo.workdir_path(&s.path).expect("path came from this worktree");
            let md = gix::index::fs::Metadata::from_path_no_follow(&abs)?;
            let (bytes, mode) =
                super::stage::read_converted_bytes(repo, filters, s.path.as_ref(), &abs, &md)?;
            s.mode = mode;
            s.id = repo.write_blob(&bytes)?.detach();
        }
        Ok(())
    }
    /// The `-n`/`-v` report, in the order git emits it: first the matched tracked
    /// entries in index order (a removed file → `remove`, a changed file — or any
    /// matched file under `-N` — → `add`, an unchanged file omitted), then the
    /// brand-new untracked files in walk order → `add`.
    fn report_lines(
        index: &gix::index::File,
        staged: &[Staged],
        deletions: &[BString],
        renormalize: bool,
    ) -> Vec<String> {
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
                // A tracked path is reported only when the worktree really differs
                // from its index entry: `add_files_to_cache()` drives the report from
                // `run_diff_files()`, which never hands an unchanged path to
                // `add_file_to_index()` at all. `-N` does not widen that — it changes
                // what gets *staged* for a path already known to differ, not which
                // paths are visited.
                //
                // `--renormalize` is the exception, and reports every matched blob:
                // `renormalize_tracked_files()` walks the index rather than a diff,
                // and `add_to_index()` skips its `alias` lookup under
                // `ADD_CACHE_RENORMALIZE` — so `was_same` is never true and the
                // `add '<path>'` line is unconditional.
                if renormalize || s.id != e.id || s.mode != e.mode {
                    lines.push(format!("add '{path}'"));
                }
            }
        }
        // `read_directory()` sorts `dir->entries` before `add_files()` walks them, so
        // the new paths are reported in path order rather than in the order the
        // directory walk happened to reach them.
        let mut fresh: Vec<&BString> =
            staged.iter().filter(|s| !s.was_tracked).map(|s| &s.path).collect();
        fresh.sort();
        for path in fresh {
            lines.push(format!("add '{path}'"));
        }
        lines
    }
    // Whether any path reached `add_to_index()` at all, which is what decides the
    // `-N` empty-blob side effect: `set_object_name_for_intent_to_add_entry()` is
    // called from inside it, so a run that indexes nothing writes nothing.
    let mut indexed_any = false;
    // The content filters git runs on the way into the object database:
    // `.gitattributes` `clean` drivers, `working-tree-encoding`, `ident`, and the
    // EOL normalization `text`/`core.autocrlf` ask for. `git add` hashes the
    // *converted* bytes, so staging the verbatim worktree copy writes a different
    // blob than git does in any repository that normalizes line endings.
    //
    // This walk stands in for git's single `index_path()` call, so it is also the
    // pass that owns the `core.autocrlf` round-trip warning and the `core.safecrlf`
    // refusal. `write_content` is exactly git's `pretend ? 0 : INDEX_WRITE_OBJECT`
    // (read-cache.c:723) plus the `intent_only` and `RENORMALIZE` arms that never
    // reach the check.
    let mut filters = super::convert_to_git::WorktreeFilter::new(&repo, write_content, renormalize)?;
    // The pipeline the deferred blob-write pass re-converts with. git converts once;
    // the check therefore stays with the scan above and is off here, or every warned
    // path would be warned about twice.
    let mut write_filters = super::convert_to_git::WorktreeFilter::new(&repo, false, renormalize)?;
    // `path_in_sparse_checkout()`: without `--sparse`, a path the sparse-checkout
    // definition leaves out of the worktree is skipped and reported instead of
    // staged. Loaded only when there is a definition to consult.
    let sparsity = sparsity_to_consult(&repo, include_sparse)?;
    let outside_sparse = |path: &BString| -> bool {
        sparsity.as_ref().is_some_and(|s| !s.includes(&path.to_str_lossy()))
    };
    // `matched_sparse_paths`: what the message at the end names, sorted and unique.
    let mut sparse_skipped: std::collections::BTreeSet<BString> = Default::default();
    // The two index-side sets the sparse accounting needs. They are deliberately
    // not the same set:
    //
    // * `skip_worktree_entries` is `PS_IGNORE_SKIP_WORKTREE`
    //   (pathspec.c:50-55, via `prune_directory()`): `seen` is computed with that
    //   flag, so an entry carrying the `skip-worktree` bit never marks a pathspec
    //   matched. It tests the BIT ONLY, and it applies whether or not `--sparse`
    //   was given.
    // * `sparse_hidden` is `find_pathspecs_matching_skip_worktree()`
    //   (pathspec.c:76-89): the bit OR a path the definition excludes. A pathspec
    //   that matched nothing else but matches one of these is
    //   `only_match_skip_worktree` (builtin/add.c:549-554) — named through
    //   `advise_on_updating_sparse_paths()` with exit 1 rather than the "did not
    //   match" fatal — and that arm is gated on `!include_sparse`, which is exactly
    //   what [`skipped_as_sparse`] folds in.
    let (skip_worktree_entries, sparse_hidden): (HashSet<BString>, HashSet<BString>) = {
        let backing = index.path_backing();
        let mut bit = HashSet::new();
        let mut hidden = HashSet::new();
        for e in index.entries().iter().filter(|e| e.stage() == Stage::Unconflicted) {
            let path = e.path_in(backing);
            if e.flags.contains(Flags::SKIP_WORKTREE) {
                bit.insert(path.to_owned());
            }
            if skipped_as_sparse(e.flags, path, include_sparse, sparsity.as_ref()) {
                hidden.insert(path.to_owned());
            }
        }
        (bit, hidden)
    };
    // Paths that could not be read, paired with the OS error text git reports
    // (only surfaced for a real add). git prints `open("<p>"): <strerror>`.
    // Paths that could not be read: `(path, strerror, was tracked)`. The third field
    // picks the fatal: a TRACKED path fails inside `update_callback()`, which dies
    // `updating files failed` (read-cache.c:3993-3995), while an untracked one fails
    // inside `add_files()`, which dies `adding files failed` (builtin/add.c:363-365).
    let mut read_errors: Vec<(BString, String, bool)> = Vec::new();
    // Embedded repositories whose HEAD is unborn: git cannot record a gitlink for
    // them and reports each one before failing the whole add.
    let mut headless_repos: Vec<BString> = Vec::new();
    // `check_embedded_repo`'s `adviced_on_embedded_repo`: the warning is printed
    // per repository, the advice at most once per invocation.
    let mut embedded_advised = false;

    // The paths git counts toward "did this pathspec match anything":
    // `prune_directory()` (builtin/add.c:99-121) marks `seen` from the walk entries
    // it kept, then adds every index entry. Filled below at the point the walk has
    // decided an entry is one git would have kept.
    let mut universe: Vec<BString> = Vec::new();

    let mut walked: Vec<gix::dir::Entry> = Vec::new();
    for item in iter.by_ref() {
        walked.push(item?.entry);
    }
    walked.sort_by_key(|e| staging_order(existing.contains(&e.rela_path), &e.rela_path));

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
        // A path outside the sparse-checkout definition is never staged, and the two
        // halves of git report it differently. `add_files()` (builtin/add.c:356-362)
        // walks the *untracked* `dir->entries` and collects each one into
        // `matched_sparse_paths`, which `advise_on_updating_sparse_paths()` names and
        // which turns the exit code into 1. `update_callback()` (read-cache.c:3979)
        // walks the tracked diff and just `continue`s — silently. Verified against git
        // 2.55.0: in a cone-sparse repo with an untracked `out/extra.txt` and a
        // restored, still-`skip-worktree` `out/f.txt`, `git add -v -A` names only
        // `out/extra.txt` and leaves `out/f.txt`'s entry untouched without a word.
        // The check sits after the eligibility filters, so a path `-u` was never going
        // to stage is not reported either.
        //
        // The seen accounting comes FIRST, because `prune_directory()` marks a
        // pathspec matched before `add_files()` ever consults the definition — which
        // is why `git add out/extra.txt` in a cone-sparse repo reports the sparse
        // block rather than "did not match any files". `PS_IGNORE_SKIP_WORKTREE`:
        // an entry carrying the bit never marks a pathspec, so it stays out.
        if !skip_worktree_entries.contains(&path) {
            universe.push(path.clone());
        }
        if outside_sparse(&path) {
            if !already_tracked {
                sparse_skipped.insert(path);
            }
            continue;
        }
        let Some(abs) = repo.workdir_path(&path) else {
            continue;
        };
        let md = gix::index::fs::Metadata::from_path_no_follow(&abs)?;
        let stat_now = Stat::from_fs(&md)?;

        // `run_diff_files(&rev, DIFF_RACY_IS_MODIFIED)` (builtin/add.c:590) picks the
        // tracked paths `add_file_to_index()` ever sees, and it picks them on the
        // **stat cache**, never on content: `ie_match_stat()` with
        // `CE_MATCH_RACY_IS_DIRTY` calls a path modified when the recorded stat
        // differs from the worktree's, and calls a racily-clean one modified whatever
        // its bytes say. A tracked path it skips is never hashed — so it is not
        // reported and cannot raise the round-trip warning. Without this gate every
        // matched tracked path was converted, which warned about a lossy CRLF file
        // git leaves alone once the racy window has passed.
        //
        // `--renormalize` replaces that walk with `renormalize_tracked_files()`'s
        // index scan (builtin/add.c:587-592), which carries no such filter.
        if already_tracked
            && !renormalize
            && !racily_clean.contains(&path)
            && tracked
                .get(&path)
                .is_some_and(|(recorded, _, _)| recorded.matches(&stat_now, stat_match))
        {
            continue;
        }

        // `-N/--intent-to-add` takes `add_to_index()`'s `intent_only` arm, which
        // skips `index_path()` entirely: nothing is read, nothing is converted and
        // nothing is hashed. The entry it builds carries the empty blob at the mode
        // the worktree stat gives, and `ADD_CACHE_NEW_ONLY` keeps it from replacing
        // an entry that already exists — so a tracked path is reported (unless
        // `was_same`) and otherwise left exactly as it was.
        if intent_to_add {
            // `set_object_name_for_intent_to_add_entry()` (read-cache.c:704-710) runs
            // for every path that reaches `add_to_index()`, before
            // `ADD_CACHE_PRETEND` is consulted — so even `-n` leaves the empty blob.
            indexed_any = true;
            staged.push(Staged {
                path,
                id: repo.object_hash().empty_blob(),
                mode: super::stage::mode_from_metadata(&md),
                stat: stat_now,
                was_tracked: already_tracked,
            });
            continue;
        }

        let (bytes, mode) = if md.is_symlink() {
            let target = match std::fs::read_link(&abs) {
                Ok(t) => t,
                Err(e) => {
                    read_errors.push((path, os_err_message(&e), already_tracked));
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
                    read_errors.push((path, os_err_message(&e), already_tracked));
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

        // `--chmod` is deliberately NOT applied here: git runs `chmod_pathspec()`
        // over the whole cache once staging is done (builtin/add.c:601-602), so the
        // mode flip reaches matched entries this walk never staged. Doing it in the
        // walk both missed those and reported every matched path as changed under
        // `-n`/`-v`, because the flipped mode always differs from the recorded one.

        // The walk only computes ids; nothing reaches the object database here. git
        // brackets all of its staging in `odb_transaction_begin()`/`_commit()`
        // (builtin/add.c:584,603) and every `die()` between them — a pathspec that
        // matched nothing, an unreadable file under `updating files failed` — leaves
        // the transaction unfinished, so the blobs it had already hashed are
        // discarded. Deferring the writes past the last of those dies is the same
        // guarantee: `git add -A` over an unreadable tracked file used to leave the
        // blobs of every file the walk reached before it in the store.
        indexed_any = true;
        let id = gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, &bytes)?;
        staged.push(Staged { path, id, mode, stat: stat_now, was_tracked: already_tracked });
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
            // A removal is `update_callback()`'s `DIFF_STATUS_DELETED` arm, which sits
            // *behind* that function's sparse guard (read-cache.c:3979-3981) — so an
            // out-of-definition path is skipped without a word here too, unlike the
            // untracked paths `add_files()` collects and names.
            if outside_sparse(&owned) {
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

    // --- validate that every pathspec matched something ---------------------
    // Runs before any object or index write, matching git: a rejected pathspec
    // leaves the repository, and the object database, completely untouched.
    //
    // The whole check is `stage`'s, because stock git dispatches both verbs to the
    // same `cmd_add()` — see [`super::stage::unmatched_pathspec_check`]. What this
    // side owns is the `seen` universe: `prune_directory()` marks a pathspec from
    // the walk entries it kept plus every index entry
    // (`add_pathspec_matches_against_index`, `PS_IGNORE_SKIP_WORKTREE`), and the
    // modes with `add_new_files == 0` get the index half alone
    // (`find_pathspecs_matching_against_index`). The walk half was collected above;
    // the rest joins it here.
    //
    // A literal prefix compare cannot stand in for this: it answers "is this path
    // at or under this element", which no wildcard element can be asked, so
    // `git add 'a/nosuch/*.txt'` used to look matched and exit 0 where git dies.
    universe.extend(existing.iter().filter(|p| !skip_worktree_entries.contains(*p)).cloned());
    // An embedded repository with an unborn HEAD matched its pathspec — it just
    // could not be indexed — and so does a gitlink the index-driven pass staged.
    universe.extend(headless_repos.iter().cloned());
    universe.extend(staged_set.iter().cloned());

    let mut seen: HashSet<usize> = HashSet::new();
    super::stage::mark_seen_per_spec(&repo, &index, &patterns, &pathspecs, &universe, &mut seen)?;
    // `if (!include_sparse && matches_skip_worktree(&pathspec, i, ...))`
    // (builtin/add.c:549-554): a pathspec that matched nothing so far but *does*
    // match an index entry the sparse-checkout definition hides is named through
    // `advise_on_updating_sparse_paths()` with exit 1, not killed with the "did not
    // match any files" fatal. `find_pathspecs_matching_skip_worktree()` asks with an
    // ordinary `ce_path_match()`, which is what `mark_seen_per_spec` runs — so it is
    // reused over the hidden paths and the pathspecs it newly marks are the ones to
    // report, named as typed (`pathspec.items[i].original`).
    let hidden: Vec<BString> = sparse_hidden.iter().cloned().collect();
    let mut hidden_seen = seen.clone();
    super::stage::mark_seen_per_spec(&repo, &index, &patterns, &pathspecs, &hidden, &mut hidden_seen)?;
    for i in hidden_seen.difference(&seen) {
        sparse_skipped.insert(BString::from(typed[*i].as_bytes()));
    }

    let check = super::stage::SpecCheck {
        original: &typed,
        resolved: &resolved,
        ignore_missing,
        ignore_errors,
        mode: match (refresh, renormalize, update_only) {
            (true, _, _) => super::stage::SpecMode::Refresh,
            (_, true, _) => super::stage::SpecMode::Renormalize,
            (_, _, true) => super::stage::SpecMode::Update,
            _ => super::stage::SpecMode::Add,
        },
    };
    // `add_files()` prints the gitignore block and sets `exit_status = 1` from
    // *inside* the odb transaction, so everything else the run matched is still
    // staged and the 1 only surfaces at `finish:`. Only a fatal stops the run.
    let mut gitignored = false;
    match super::stage::unmatched_pathspec_check(&repo, &index, &check, &hidden_seen)? {
        super::stage::SpecVerdict::Fatal(code) => return Ok(code),
        // `report_path_error()` exits from inside the odb transaction, so everything
        // `add_files_to_cache()` already did stays: its `-v`/`-n` lines are on stdout
        // and the blobs it hashed are in the store. Only the index write, which
        // `finish:` owns, is skipped.
        super::stage::SpecVerdict::Unknown(code) => {
            if dry_run || verbose {
                for line in report_lines(&index, &staged, &deletions, renormalize) {
                    println!("{line}");
                }
            }
            if write_content {
                deposit_staged_blobs(&repo, &mut staged, &mut write_filters)?;
            }
            if intent_to_add && indexed_any {
                repo.write_blob(b"")?;
            }
            return Ok(code);
        }
        super::stage::SpecVerdict::Ignored => gitignored = true,
        super::stage::SpecVerdict::Ok => {}
    }

    // `--refresh` only refreshes the stat cache (invisible to the object/ref/index
    // logical state) and never adds content: nothing more to write here.
    if refresh {
        return Ok(ExitCode::SUCCESS);
    }

    // `--renormalize` re-stages tracked content off the *index* rather than off the
    // walk above, and does all of this command's object writing while it is at it.
    // Shared verbatim with the `stage` verb — see [`renormalize_tracked_files`].
    let mut index_failed = false;
    if renormalize {
        let outcome = renormalize_tracked_files(
            &repo,
            &index,
            &RenormalizeOpts { include_sparse, dry_run, verbose, intent_to_add },
            &mut filters,
            |p| pathspec.is_included(p, Some(false)),
        )?;
        if let Some(code) = outcome.aborted {
            return Ok(code);
        }
        index_failed = outcome.failed;
    }

    // `--ignore-errors`: a real add reports the paths it could not index and, if
    // any occurred without `--ignore-errors`, aborts before touching the index.
    // An embedded repository with an unborn HEAD is one of those paths; git names
    // it with the trailing slash the directory walk carries.
    //
    // `--renormalize` never gets here: `cmd_add()` runs `renormalize_tracked_files()`
    // *instead of* `add_files_to_cache()` and skips `add_files()` entirely
    // (`add_new_files` is 0), so under that flag neither of the two callers that
    // report an unindexable path exists, and the scan above owns the whole story.
    let had_errors = !renormalize && !(read_errors.is_empty() && headless_repos.is_empty());
    // `--dry-run` does not suppress any of this: `ADD_CACHE_PRETEND` is consulted
    // inside `add_to_index()` only after `index_path()` has already failed, so `-n`
    // reports the same two `error:` lines and dies the same way. Verified against git
    // 2.55.0: `git add -n --ignore-errors -A` over an unreadable tracked file prints
    // both lines and exits 1.
    if had_errors {
        for p in &headless_repos {
            eprintln!("error: '{p}/' does not have a commit checked out");
            eprintln!("error: unable to index file '{p}/'");
        }
        for (p, msg, _) in &read_errors {
            eprintln!("error: open(\"{p}\"): {msg}");
            eprintln!("error: unable to index file '{p}'");
        }
        if !ignore_errors {
            // `add_files_to_cache()` runs first and dies on the first tracked path it
            // could not index, so a tracked failure always outranks an untracked one.
            let verb = if read_errors.iter().any(|(_, _, tracked)| *tracked) {
                "updating"
            } else {
                "adding"
            };
            eprintln!("fatal: {verb} files failed");
            return Ok(ExitCode::from(128));
        }
    }

    // --- object writes: everything git does inside its odb transaction ------
    // `odb_transaction_begin()` opens at builtin/add.c:584, after the pathspec
    // checks, and `odb_transaction_commit()` closes at :603. Every `die()` in
    // between — including `updating files failed` just above — leaves it
    // unfinished, so the blobs already hashed never land. The walk therefore only
    // computed ids; they are deposited here, past the last of those dies.
    //
    // `report_path_error()` is the one exit that is *not* a die and does leave them
    // behind — it has its own call above, on the `Unknown` verdict.
    if write_content {
        deposit_staged_blobs(&repo, &mut staged, &mut write_filters)?;
    }

    // `-N` reaches `set_object_name_for_intent_to_add_entry()` for every path
    // `add_files_to_cache()` and `add_files()` actually index, and that helper
    // writes the empty blob before `add_to_index()` ever looks at
    // `ADD_CACHE_PRETEND` — so `--dry-run` leaves the object behind too. A
    // pathspec that matched only unchanged tracked paths indexes nothing and
    // therefore writes nothing.
    let intent_visited = intent_to_add && indexed_any;
    if intent_visited {
        repo.write_blob(b"")?;
    }

    // Build the `-n`/`-v` report exactly as git orders it: first the matched
    // tracked entries in index order (a removed file → `remove`, a changed file
    // — or any matched file under `-N` — → `add`, an unchanged file omitted),
    // then the brand-new untracked files in walk order → `add`.
    let report: Vec<String> = match dry_run || verbose {
        false => Vec::new(),
        true => report_lines(&index, &staged, &deletions, renormalize),
    };

    // `if (chmod_arg && pathspec.nr)` (builtin/add.c:601): the mode override runs
    // only when a pathspec was given, so `-A`/`-u` — which synthesize none — leave
    // every mode alone. Verified against git 2.55.0: `git add -u --chmod=+x` keeps
    // a tracked file at 100644.
    let chmod = if pathspecs.is_empty() { None } else { chmod };

    // --- dry run: report only, never touch the index ------------------------
    // `chmod_pathspec(show_only = 1)` still runs, and still answers the `S_ISREG`
    // question for every matched entry — a `--dry-run` over a matched symlink
    // reports `cannot chmod +x` and exits 255 exactly as the real run does.
    if dry_run {
        for line in &report {
            println!("{line}");
        }
        let chmod_errors = match chmod {
            None => Vec::new(),
            Some(flip) => {
                chmod_pathspec(&index, flip, include_sparse, sparsity.as_ref(), |p| {
                    pathspec.is_included(p, Some(false))
                })
                .1
            }
        };
        return Ok(finish_code(Finish {
            had_errors,
            ignore_errors,
            gitignored,
            sparse_skipped: &sparse_skipped,
            chmod_errors: &chmod_errors,
            index_failed,
        }));
    }

    // Nothing to write: leave the index file alone so its extensions survive a run
    // that changed nothing. Under `-N` only the brand-new paths become entries — a
    // tracked one is `ADD_CACHE_NEW_ONLY`, i.e. reported and then left as it was —
    // so a `-N` run that matched only tracked paths is one of these.
    let index_entries = if intent_to_add {
        staged.iter().filter(|s| !s.was_tracked).count()
    } else {
        staged.len()
    };
    if index_entries == 0 && deletions.is_empty() && chmod.is_none() {
        // The report still comes out: git prints `add '<path>'` from inside
        // `add_to_index()` as it goes, whether or not the cache ends up changing.
        if verbose {
            for line in &report {
                println!("{line}");
            }
        }
        return Ok(finish_code(Finish {
            had_errors,
            ignore_errors,
            gitignored,
            sparse_skipped: &sparse_skipped,
            chmod_errors: &[],
            index_failed,
        }));
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
        let chmod_errors = apply_chmod_pathspec(&mut index, chmod, include_sparse, sparsity.as_ref(), |p| {
            pathspec.is_included(p, Some(false))
        });
        invalidate_tree_cache(&mut index, &remove);
        // `do_write_index()` sets the hashfile's `skip_hash` from the repository's
        // settings block before it serialises a single entry
        // (read-cache.c:2830-2831), so `index.skipHash` — and the
        // `feature.manyFiles` macro that defaults it (repo-settings.c:59-63, then
        // `:81`) — governs every index git writes, whichever verb wrote it.
        // Going through the shared reader is what keeps the trailer this leaves
        // behind identical to the one `update-index` would have left.
        //
        // `IEOT` is the same kind of repository-wide decision and is made in the
        // same function, one block earlier: `if (nr_threads != 1 &&
        // record_ieot())` (read-cache.c:2877-2904) is evaluated before any entry
        // is written, and the extension goes out ahead of every other one
        // (`:2983-2993`). In C that reaches `cmd_add` for free because it writes
        // through `write_locked_index()` (read-cache.c:3323) like everything else;
        // here the decision lives outside the writer, so each caller has to attach
        // it — and without this line `add` was the verb that dropped `IEOT`, and
        // with it `EOIE`, which gix only emits alongside another extension.
        super::write_tree::prepare_offset_table(&repo, &mut index);
        crate::index_racy::write(&repo, &mut index)?;
        record_stage_event(&repo, staged.len() + deletions.len());

        if verbose {
            for line in &report {
                println!("{line}");
            }
        }
        return Ok(finish_code(Finish {
            had_errors,
            ignore_errors,
            gitignored,
            sparse_skipped: &sparse_skipped,
            chmod_errors: &chmod_errors,
            index_failed,
        }));
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

    // `chmod_pathspec()` runs last (builtin/add.c:601-602), over the cache staging
    // has already updated — which is what makes the flip reach a matched entry this
    // run never restaged.
    let chmod_errors = apply_chmod_pathspec(&mut index, chmod, include_sparse, sparsity.as_ref(), |p| {
        pathspec.is_included(p, Some(false))
    });

    invalidate_tree_cache(&mut index, &remove);
    // Same options — and the same `IEOT` decision — as the intent-to-add path
    // above: what an index carries beyond its entries is a property of the
    // repository, not of the verb that wrote it (read-cache.c:2830-2831 for the
    // trailer, `:2877-2904` for the offset table).
    super::write_tree::prepare_offset_table(&repo, &mut index);
    crate::index_racy::write(&repo, &mut index)?;
    record_stage_event(&repo, staged.len() + deletions.len());

    if verbose {
        for line in &report {
            println!("{line}");
        }
    }

    Ok(finish_code(Finish {
        had_errors,
        ignore_errors,
        gitignored,
        sparse_skipped: &sparse_skipped,
        chmod_errors: &chmod_errors,
        index_failed,
    }))
}

/// The sparse-checkout definition `path_in_sparse_checkout()` (sparse-index.c)
/// consults, or `None` when there is nothing to consult.
///
/// `None` covers both of git's short circuits: `--sparse` (git's `include_sparse`)
/// skips every such test outright, and a repository without `core.sparseCheckout`
/// has no definition, which `path_in_sparse_checkout()` answers by returning true
/// for every path. Note that it does NOT cover `ce_skip_worktree()`, which is a bit
/// on the index entry and is consulted whether or not a definition exists.
pub(super) fn sparsity_to_consult(
    repo: &gix::Repository,
    include_sparse: bool,
) -> Result<Option<super::sparse_checkout::Sparsity>> {
    if include_sparse || !repo.config_snapshot().boolean("core.sparseCheckout").unwrap_or(false) {
        return Ok(None);
    }
    Ok(Some(super::sparse_checkout::load_sparsity(repo)?))
}

/// git's `!include_sparse && (ce_skip_worktree(ce) || !path_in_sparse_checkout(...))`
/// — the guard `renormalize_tracked_files()` and `chmod_pathspec()` (builtin/add.c)
/// each open their cache loop with, character for character the same in both.
///
/// The two halves are not interchangeable. `ce_skip_worktree()` reads a bit off the
/// index entry, which `update-index --skip-worktree` can set with
/// `core.sparseCheckout` off, so it is consulted even when `sparsity` is `None`;
/// `path_in_sparse_checkout()` consults the definition, which is what `sparsity`
/// carries. Testing only the second is what let `stage --renormalize` die on a
/// `--skip-worktree` entry that git skips.
pub(super) fn skipped_as_sparse(
    flags: Flags,
    path: &BStr,
    include_sparse: bool,
    sparsity: Option<&super::sparse_checkout::Sparsity>,
) -> bool {
    !include_sparse
        && (flags.contains(Flags::SKIP_WORKTREE)
            || sparsity.is_some_and(|s| !s.includes(&path.to_str_lossy())))
}

/// Port of `chmod_pathspec()` (builtin/add.c:42-71): decide which index entries
/// `--chmod=(+|-)x` flips, and which ones refuse.
///
/// The scan is driven by the **cache**, not by whatever the worktree walk staged,
/// and that is the whole point of the function: `git add --chmod=+x a.txt` over an
/// unchanged, already-tracked `a.txt` still rewrites its mode to 100755 (verified
/// against git 2.55.0), and so does `git add -N --chmod=+x a.txt`, which stages no
/// content at all. An implementation that flips the mode inside the staging walk
/// misses every matched entry the walk did not restage — and, under `-n`/`-v`,
/// reports every matched path as changed, because the flipped mode never equals the
/// recorded one.
///
/// `chmod_index_entry()` (read-cache.c:907-911) returns -1 for anything that is not
/// a regular file, and the `show_only` arm asks the same `S_ISREG` question without
/// touching the entry — so a matched symlink or gitlink is `error: cannot chmod +x
/// '<path>'` under `--dry-run` too. `error()` is -1, `ret` is -1, `exit_status |= -1`
/// is -1, and `cmd_add()` returning -1 is what the shell reads as 255.
///
/// Returns `(paths to flip, error lines in cache order)`; the caller applies the
/// first only when this is a real run.
pub(super) fn chmod_pathspec(
    index: &gix::index::File,
    flip: bool,
    include_sparse: bool,
    sparsity: Option<&super::sparse_checkout::Sparsity>,
    mut selected: impl FnMut(&BStr) -> bool,
) -> (HashSet<BString>, Vec<String>) {
    let sign = if flip { '+' } else { '-' };
    let mut wanted: HashSet<BString> = HashSet::new();
    let mut errors: Vec<String> = Vec::new();
    let backing = index.path_backing();
    for e in index.entries() {
        let path = e.path_in(backing);
        // The very same guard `renormalize_tracked_files()` opens with; see
        // [`skipped_as_sparse`]. Without it, `--chmod=+x .` flipped the mode of every
        // entry the definition (or a `--skip-worktree` bit) keeps out of the worktree.
        if skipped_as_sparse(e.flags, path, include_sparse, sparsity) {
            continue;
        }
        // `ce_path_match()`: every stage, not just stage 0 — the C loop walks the
        // raw cache and has no `ce_stage()` filter.
        if !selected(path) {
            continue;
        }
        if matches!(e.mode, Mode::FILE | Mode::FILE_EXECUTABLE) {
            wanted.insert(path.to_owned());
        } else {
            errors.push(format!("error: cannot chmod {sign}x '{path}'"));
        }
    }
    (wanted, errors)
}

/// [`chmod_pathspec`] for a real run: decide, then flip. Split in two because the
/// decision borrows the index (path backing and matcher) while the flip needs it
/// exclusively. `None` is git's `chmod_arg == NULL`, which skips the pass entirely.
pub(super) fn apply_chmod_pathspec(
    index: &mut gix::index::File,
    chmod: Option<bool>,
    include_sparse: bool,
    sparsity: Option<&super::sparse_checkout::Sparsity>,
    selected: impl FnMut(&BStr) -> bool,
) -> Vec<String> {
    let Some(flip) = chmod else { return Vec::new() };
    let (wanted, errors) = chmod_pathspec(index, flip, include_sparse, sparsity, selected);
    if !wanted.is_empty() {
        let want = if flip { Mode::FILE_EXECUTABLE } else { Mode::FILE };
        for (entry, path) in index.entries_mut_with_paths() {
            if wanted.contains(&path.to_owned()) {
                entry.mode = want;
            }
        }
        // A mode flip changes the tree entry, so `chmod_index_entry()` invalidates
        // the cache-tree along that path just like a content change does
        // (read-cache.c:935). Missing this would leave a cached subtree id that
        // still describes the old mode.
        invalidate_tree_cache(index, &wanted);
    }
    errors
}

/// Invalidate the tree-cache along every path in `paths` — git's
/// `cache_tree_invalidate_path()` (cache-tree.c:159-163), which it calls once per
/// entry from inside `add_index_entry_with_check()` (read-cache.c:1273-1274) and
/// `remove_file_from_index()` (read-cache.c:632).
///
/// Doing it after the entries were replaced rather than during is equivalent:
/// invalidation reads nothing from the entries, it only marks the nodes on the
/// path — and removes the node named by the final component — as needing
/// recomputation. What matters is that the set is complete, since a path that is
/// changed without being invalidated leaves a cached tree id describing content
/// that is no longer in the index.
pub(super) fn invalidate_tree_cache(index: &mut gix::index::File, paths: &HashSet<BString>) {
    for path in paths {
        index.invalidate_path_in_tree(path.as_ref());
    }
}

/// What [`renormalize_tracked_files`] reads off the command line.
///
/// `git stage` is registered in git's command table as `cmd_add` itself
/// (`git-stage(1)`: "This is a synonym for git-add(1)"), so nothing here is
/// per-verb: both verbs fill it from the same option table and get the same scan.
pub(super) struct RenormalizeOpts {
    /// `--sparse` (git's `include_sparse`).
    pub include_sparse: bool,
    /// `-n` (`ADD_CACHE_PRETEND`).
    pub dry_run: bool,
    /// `-v` (`ADD_CACHE_VERBOSE`).
    pub verbose: bool,
    /// `-N` (`add_to_index()`'s `intent_only` branch).
    pub intent_to_add: bool,
}

/// Port of `renormalize_tracked_files()` (builtin/add.c), shared by `add` and
/// `stage` because stock git dispatches both verbs to the very same `cmd_add()`.
///
/// Under `--renormalize`, this scan *replaces* `add_files_to_cache()` — `cmd_add()`
/// runs one or the other, never both — so it is also where every content object of
/// such a run is written. It walks the cache in entry order, applies git's four
/// filters (sparse, unmerged, non-blob, pathspec) and hands each survivor to
/// `add_file_to_index()`, which `lstat`s the worktree path and `die_errno`s when it
/// is gone. A matched tracked path whose file was deleted therefore aborts the
/// command rather than staging the removal — independently of `--ignore-removal`
/// (that flag only governs `update_callback()`'s removal arm, which `--renormalize`
/// never reaches) and of `--ignore-errors` (the `die` is inside
/// `add_file_to_index()`, not a returned error).
///
/// The abort happens *mid-walk*, which is observable three ways and is why the
/// order of the writes below matters:
///
/// * the entries ahead of the missing one have already printed their `add '<path>'`
///   line, so `--renormalize -n --all` over a tree with `README.md` modified and
///   `src/lib.rs` deleted prints `add 'README.md'` and *then* the fatal. `-n` and
///   `-v` are what turn those lines on at all; without either the abort prints
///   nothing to stdout.
/// * their blobs are already in the object store, and the ones behind it never get
///   there. `add_to_index()` reaches `index_path()`, which hashes *and writes*
///   before recording the entry.
/// * under `-N` the empty blob is there too:
///   `set_object_name_for_intent_to_add_entry()` runs before `add_to_index()` ever
///   consults `ADD_CACHE_PRETEND`, so even `--dry-run` deposits it — but only once
///   at least one entry got through ahead of the missing path.
///
/// `ADD_CACHE_RENORMALIZE` skips `add_to_index()`'s `alias` lookup, so `was_same` is
/// never true and an unchanged blob is handled (and reported) too. `selected` is the
/// caller's `ce_path_match()`: the two verbs normalize their pathspecs differently
/// and so carry different matchers, but ask them the same question.
///
/// A file that cannot be *read* is the other failure, and it is nothing like the
/// missing one: `index_path()` fails, `add_to_index()` reports
/// `error: unable to index file '<path>'` and returns -1, and the loop's
/// `retval |= …` carries that to the end while the scan **keeps going** — later
/// entries are still indexed, still reported, and the index is still written.
/// `cmd_add()` then returns -1, which the shell reads as 255, and `--ignore-errors`
/// changes none of it: that flag is consulted by `update_callback()` and
/// `add_files()`, neither of which runs under `--renormalize`. Verified against git
/// 2.55.0: `git add -v --renormalize -A` over an unreadable `u.txt` prints the two
/// `error:` lines, then `add 'a.txt'` and `add 'z.txt'`, updates both of their index
/// entries, leaves `u.txt`'s alone, and exits 255.
///
/// `index_path()` runs under `--dry-run` too — `hash_flags` merely loses
/// `INDEX_WRITE_OBJECT` (read-cache.c:723) — so `-n` fails the same entry. `-N` takes
/// the `intent_only` branch instead and never reads the file at all.
pub(super) struct RenormalizeOutcome {
    /// `add_file_to_index()`'s `die_errno` on a vanished path: the scan stopped
    /// there and this is the whole command's exit code.
    pub aborted: Option<ExitCode>,
    /// At least one entry could not be indexed — git's `exit_status |= -1`.
    pub failed: bool,
}

/// Returns how the scan ended; see [`RenormalizeOutcome`].
pub(super) fn renormalize_tracked_files(
    repo: &gix::Repository,
    index: &gix::index::File,
    opts: &RenormalizeOpts,
    filters: &mut super::convert_to_git::WorktreeFilter,
    mut selected: impl FnMut(&BStr) -> bool,
) -> Result<RenormalizeOutcome> {
    let sparsity = sparsity_to_consult(repo, opts.include_sparse)?;
    // The paths already handed to `add_file_to_index()`, in index order.
    let mut handled: Vec<BString> = Vec::new();
    let mut failed = false;
    let backing = index.path_backing();
    for e in index.entries() {
        let path = e.path_in(backing);
        // A path the sparse-checkout definition keeps out of the worktree is absent
        // by design, so the scan skips it instead of dying on the failed `lstat`.
        if skipped_as_sparse(e.flags, path, opts.include_sparse, sparsity.as_ref()) {
            continue;
        }
        if e.stage() != Stage::Unconflicted {
            continue; // "do not touch unmerged paths"
        }
        if !matches!(e.mode, Mode::FILE | Mode::FILE_EXECUTABLE | Mode::SYMLINK) {
            continue; // "do not touch non blobs"
        }
        if !selected(path) {
            continue;
        }
        let abs = repo.workdir_path(path);
        let Some(abs) = abs.filter(|p| std::fs::symlink_metadata(p).is_ok()) else {
            if opts.dry_run || opts.verbose {
                for done in &handled {
                    println!("add '{done}'");
                }
            }
            if opts.intent_to_add && !handled.is_empty() {
                repo.write_blob(b"")?;
            }
            eprintln!("fatal: unable to stat '{path}': No such file or directory");
            return Ok(RenormalizeOutcome { aborted: Some(ExitCode::from(128)), failed });
        };
        // `-N` takes `add_to_index()`'s `intent_only` branch, which never calls
        // `index_path()` — the file is not even opened, so it cannot fail here.
        if opts.intent_to_add {
            handled.push(path.to_owned());
            continue;
        }
        // `index_path()` runs for `--dry-run` as well; `hash_flags` only drops
        // `INDEX_WRITE_OBJECT`. So the read happens either way and only the *write*
        // is conditional.
        let read = gix::index::fs::Metadata::from_path_no_follow(&abs)
            .map_err(|e| e.to_string())
            .and_then(|md| {
                super::stage::read_converted_bytes(repo, filters, path, &abs, &md)
                    .map_err(|e| e.to_string())
            });
        let (content, _) = match read {
            Ok(v) => v,
            Err(msg) => {
                // `index_path()`'s `error_errno("open(\"%s\")")` and
                // `add_to_index()`'s `error(_("unable to index file '%s'"))`
                // (read-cache.c:784). The scan does not stop.
                eprintln!("error: open(\"{path}\"): {}", strip_os_error(&msg));
                eprintln!("error: unable to index file '{path}'");
                failed = true;
                continue;
            }
        };
        handled.push(path.to_owned());
        if !opts.dry_run {
            repo.write_blob(&content)?;
        }
    }
    Ok(RenormalizeOutcome { aborted: None, failed })
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

/// Where the path starts inside `spec`, and whether its magic says `top`.
///
/// `parse_short_magic()` (pathspec.c:365-398) walks the characters after the
/// leading `:` while they are magic mnemonics — only `/` (top) and `!`/`^`
/// (exclude) have one — and steps over a terminating `:`. The long form
/// (`parse_long_magic`) is a comma-separated list up to `)`. `prefix:N` counts as
/// "already prefixed" for the same reason `top` does: `prefix_pathspec()` takes
/// the element verbatim in both cases (pathspec.c:452-457).
fn split_pathspec_magic(spec: &str) -> (usize, bool) {
    let b = spec.as_bytes();
    if b.first() != Some(&b':') {
        return (0, false);
    }
    if b.get(1) == Some(&b'(') {
        // A missing `)` is git's "Missing ')' at the end of pathspec magic", which
        // the matcher below reports; treat the element as all-magic so this pass
        // leaves it exactly as typed.
        let Some(close) = spec[2..].find(')').map(|i| i + 2) else {
            return (spec.len(), true);
        };
        let rooted =
            spec[2..close].split(',').any(|m| m == "top" || m.starts_with("prefix:"));
        (close + 1, rooted)
    } else {
        let mut i = 1;
        let mut rooted = false;
        while i < b.len() && b[i] != b':' {
            match b[i] {
                b'/' => rooted = true,
                b'!' | b'^' => {}
                // Not a mnemonic: the path starts here.
                _ => break,
            }
            i += 1;
        }
        if i < b.len() && b[i] == b':' {
            i += 1;
        }
        (i, rooted)
    }
}

/// `parse_pathspec()`'s prefix pass (pathspec.c:455-467, via `prefix_path_gently`)
/// applied to a whole element list, shared by `add` and `stage` because stock git
/// dispatches both verbs to the same `cmd_add()`.
///
/// Each element comes back as a pair: `pathspec.items[i].original`, the element
/// byte for byte **as typed**, which is what git quotes back in its diagnostics
/// (`git add sub/` names `'sub/'`, not the normalized `'sub'`), and
/// `pathspec.items[i].match`, the **repo-relative** form, which is the only one that
/// can be compared with index and worktree paths — those are always
/// repo-relative. `pathspecs` itself is left holding the normalized text. Skipping
/// the second is why `git add f.txt` in a subdirectory once reported `pathspec
/// 'f.txt' did not match any files` for a file the walk had just staged, and why
/// `git stage sub/f.txt` from `sub/` matched the repository's `sub/f.txt` instead of
/// the `sub/sub/f.txt` git looks for.
///
/// `pathspecs` itself is left as typed — gitoxide resolves the patterns against the
/// repository prefix on its own, so handing it a prefixed element would apply the
/// prefix twice — with one exception, rewritten in place: an absolute element, which
/// the matcher cannot take at all.
///
/// An element that resolves outside the worktree is `prefix_path_gently()` returning
/// NULL, which is a die, not a skip. It exits the process (128), as git's does.
/// `die_path_inside_submodule()` (submodule.c:340-368):
///
/// ```c
/// for (i = 0; i < istate->cache_nr; i++) {
///         struct cache_entry *ce = istate->cache[i];
///         int ce_len = ce_namelen(ce);
///
///         if (!S_ISGITLINK(ce->ce_mode))
///                 continue;
///
///         for (j = 0; j < ps->nr ; j++) {
///                 const struct pathspec_item *item = &ps->items[j];
///
///                 if (item->len <= ce_len)
///                         continue;
///                 if (item->match[ce_len] != '/')
///                         continue;
///                 if (strncmp(ce->name, item->match, ce_len))
///                         continue;
///                 if (item->len == ce_len + 1)
///                         continue;
///
///                 die(_("Pathspec '%s' is in submodule '%.*s'"),
///                     item->original, ce_len, ce->name);
///         }
/// }
/// ```
///
/// A path *inside* a submodule is not this repository's to stage, and git says
/// so rather than letting the walk call it untracked. The trailing-slash form is
/// deliberately let through — `item->len == ce_len + 1` is exactly `sub/`, which
/// names the gitlink itself and is a no-op, not an error.
///
/// The message quotes the spec as it was typed and the gitlink as the index
/// records it; the first *index* entry that matches any spec is the one that
/// dies, which is index order rather than command-line order.
fn die_path_inside_submodule(
    index: &gix::index::File,
    typed: &[String],
    resolved: &[String],
) -> Option<ExitCode> {
    let backing = index.path_backing();
    for entry in index.entries() {
        if entry.mode != Mode::COMMIT {
            continue;
        }
        let name = entry.path_in(backing);
        for (original, matched) in typed.iter().zip(resolved) {
            let matched = matched.as_bytes();
            if matched.len() <= name.len()
                || matched.get(name.len()) != Some(&b'/')
                || matched[..name.len()] != name[..]
                || matched.len() == name.len() + 1
            {
                continue;
            }
            eprintln!("fatal: Pathspec '{original}' is in submodule '{name}'");
            return Some(ExitCode::from(128));
        }
    }
    None
}

pub(super) fn resolve_pathspecs(
    repo: &gix::Repository,
    pathspecs: &mut [String],
) -> Result<Vec<(String, String)>> {
    // `init_pathspec_item()` keeps `item->original = xstrdup(elt)` — the element
    // byte for byte as typed — and normalizes only the copy it matches with. Every
    // diagnostic quotes `original`, so it has to be captured before the rewrite.
    let original: Vec<String> = pathspecs.to_vec();
    // `prefix_path()` runs every command-line path through `normalize_path_copy()`
    // first, so `./.` is `.`, `src/.` is `src`, and `a/../b` is `b` before anything
    // asks whether the path exists or is ignored.
    for spec in pathspecs.iter_mut() {
        *spec = normalize_pathspec(spec);
    }
    // git's `prefix` is directory-terminated ("sub/") while gix reports it without
    // the separator; concatenating that would produce `subf.txt`.
    let prefix = repo
        .prefix()
        .ok()
        .flatten()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .filter(|p| !p.is_empty())
        .map(|p| if p.ends_with('/') { p } else { format!("{p}/") })
        .unwrap_or_default();
    let mut checked: Vec<(String, String)> = Vec::with_capacity(pathspecs.len());
    for i in 0..pathspecs.len() {
        let spec = pathspecs[i].clone();
        match prefixed_pathspec(&spec, &prefix, repo) {
            Ok(relative) => {
                // An absolute pathspec is the one case the matcher cannot take as
                // typed — gitoxide reads it as a path outside the worktree and
                // refuses. Its repo-relative form cannot be handed over bare
                // either, since the walk would resolve THAT against the prefix a
                // second time; `:(top)` is the spelling that says "already rooted".
                if spec.starts_with('/') {
                    pathspecs[i] = format!(":(top){relative}");
                }
                checked.push((original[i].clone(), relative));
            }
            Err(copyfrom) => {
                // `absolute_path(hint_path)`: the worktree gix hands back can be
                // relative to the cwd ("..") and git always names an absolute one.
                let root = repo
                    .workdir()
                    .unwrap_or_else(|| repo.git_dir())
                    .canonicalize()
                    .unwrap_or_else(|_| repo.git_dir().to_path_buf())
                    .display()
                    .to_string();
                crate::git_fatal!("{spec}: '{copyfrom}' is outside repository at '{root}'");
            }
        }
    }
    Ok(checked)
}

/// The order `cmd_add()` visits paths in, as a sort key over the worktree walk.
///
/// git stages in two passes: `add_files_to_cache()` goes first and drives
/// `update_callback()` off `run_diff_files()`, which iterates the *index* — so the
/// tracked matches come out in path order — and `add_files()` goes second over the
/// sorted `dir->entries` (builtin/add.c:356-370, 590-598) for the brand-new ones.
///
/// Everything a staging pass emits per path comes out in that order, not just the
/// `-v`/`-n` report: the `core.autocrlf` round-trip warnings and the
/// `error: unable to index file` pairs too. `stage` used to walk in the directory
/// order gitoxide handed it, which put an unreadable `u.txt`'s two error lines
/// ahead of `crlf.txt`'s warning where git has them the other way round.
pub(super) fn staging_order(already_tracked: bool, path: &BString) -> (bool, BString) {
    (!already_tracked, path.clone())
}

/// `common_prefix_len()` (dir.c:215-259): the longest directory prefix shared by
/// every non-exclude pathspec, always `/`-terminated, or `None` when there is none.
///
/// `fill_directory()` (dir.c:287-291) hands that prefix to `read_directory()` as the
/// place to start the untracked-file walk, which is what makes the walk — and its
/// diagnostics — talk about a directory nobody typed.
///
/// Each element is compared in its repo-relative form (`pathspec.items[i].match`)
/// and only up to its `nowildcard_len`, so a wildcard truncates the prefix at the
/// last `/` before it. An element that carries magic is out of scope here: git
/// computes `nowildcard_len` from the parsed remainder and `:(icase)` switches to
/// `item->prefix` entirely, so rather than approximate either, the whole
/// computation bails — which yields no prefix, hence no diagnostic, which is the
/// conservative answer.
fn pathspec_common_prefix(typed: &[String], resolved: &[String]) -> Option<String> {
    if typed.is_empty() || typed.iter().any(|s| s.starts_with(':')) {
        return None;
    }
    let first = resolved.first()?.as_bytes();
    let mut max = 0usize;
    for (n, spec) in resolved.iter().enumerate() {
        let b = spec.as_bytes();
        let item_len = nowildcard_len(b);
        let mut len = 0usize;
        let mut i = 0usize;
        while i < item_len && (n == 0 || i < max) {
            let Some(&c) = b.get(i) else { break };
            if first.get(i) != Some(&c) {
                break;
            }
            if c == b'/' {
                len = i + 1;
            }
            i += 1;
        }
        if n == 0 || len < max {
            max = len;
            if max == 0 {
                return None;
            }
        }
    }
    (max > 0).then(|| String::from_utf8_lossy(&first[..max]).into_owned())
}

/// `simple_length()` (pathspec.c): how much of a pathspec is literal, i.e. the
/// offset of the first `is_glob_special()` byte.
fn nowildcard_len(spec: &[u8]) -> usize {
    spec.iter()
        .position(|c| matches!(c, b'*' | b'?' | b'[' | b'\\'))
        .unwrap_or(spec.len())
}

/// `read_directory()`'s `opendir()` failure on the pathspec's common prefix
/// (dir.c:2585-2587, reached through dir.c:3156-3157).
///
/// `git add sub/f.txt` run from `sub/` asks for `sub/sub/f.txt`, whose common
/// prefix `sub/sub/` does not exist — so the untracked walk warns before the
/// pathspec check gets its chance to die. gitoxide's dirwalk starts from the
/// worktree root and simply matches, so it never opens that directory and never
/// has anything to report; the diagnostic is reproduced here instead, from the
/// same prefix `fill_directory()` computes.
///
/// `treat_leading_path()` (dir.c:2811-2880) decides whether the walk starts at all:
/// it descends the prefix one component at a time and stops at the first one that
/// is not a directory, returning "recurse" only if it had already descended into
/// one. So `nosuch/x` never opens anything (nothing to warn about) while
/// `sub/nosuch/x` does (`sub` is a directory, `sub/nosuch/` is not) — which is
/// exactly the difference between a silent "did not match" and this warning.
pub(super) fn warn_unopenable_walk_prefix(
    repo: &gix::Repository,
    typed: &[String],
    resolved: &[String],
) {
    let Some(prefix) = pathspec_common_prefix(typed, resolved) else {
        return;
    };
    let Some(root) = repo.workdir() else { return };
    // `treat_leading_path()`: descend while each component is a directory. The walk
    // only starts once at least one component has been descended into.
    let components: Vec<&str> = prefix.trim_end_matches('/').split('/').collect();
    let mut descended = false;
    for i in 0..components.len() {
        if !root.join(components[..=i].join("/")).is_dir() {
            break;
        }
        descended = true;
    }
    if !descended {
        return;
    }
    // `opendir(prefix)`. The whole prefix being a directory is the ordinary case and
    // says nothing; only a failure is reported, with `strerror(errno)` as git's
    // `warning_errno()` renders it.
    if let Err(e) = std::fs::read_dir(root.join(prefix.trim_end_matches('/'))) {
        eprintln!("warning: could not open directory '{prefix}': {}", os_err_message(&e));
    }
}

/// `prefix_path_gently()` (setup.c:103-130) applied to one pathspec element,
/// keeping its magic.
///
/// Relative paths are resolved against `prefix` — that is what makes `git add
/// f.txt` in `sub/` mean `sub/f.txt` — and absolute ones are cut down to the part
/// inside the worktree (`abspath_part_inside_repo`). `Err` carries the path git
/// names in its "is outside repository" die: the element resolved above the
/// worktree root, which `normalize_path_copy_len()` reports by failing.
fn prefixed_pathspec(
    spec: &str,
    prefix: &str,
    repo: &gix::Repository,
) -> Result<String, String> {
    let (path_at, rooted) = split_pathspec_magic(spec);
    let (magic, path) = spec.split_at(path_at);
    // `:/` and `:(top)` are precisely the request to skip the prefix, and an
    // element that is nothing but magic has no path to resolve.
    if rooted || path.is_empty() {
        return Ok(spec.to_string());
    }

    let normalized = if path.starts_with('/') {
        // `abspath_part_inside_repo()` compares REALPATHS (`strbuf_realpath`), and
        // it has to: on macOS the worktree reached through `$TMPDIR` is `/var/…`
        // while its real path is `/private/var/…`, so a plain string compare calls
        // a path inside the repo an outside one. The file itself need not exist —
        // `git add` names paths that do not — so an unresolvable leaf falls back to
        // resolving its directory.
        let real = |p: &std::path::Path| -> std::path::PathBuf {
            p.canonicalize().unwrap_or_else(|_| match (p.parent(), p.file_name()) {
                (Some(dir), Some(name)) => {
                    dir.canonicalize().map(|d| d.join(name)).unwrap_or_else(|_| p.to_path_buf())
                }
                _ => p.to_path_buf(),
            })
        };
        let normalized = real(std::path::Path::new(&normalize_pathspec(path)))
            .to_string_lossy()
            .replace('\\', "/");
        let root = repo.workdir().ok_or_else(|| path.to_string())?;
        let root = real(root).to_string_lossy().replace('\\', "/");
        // Only a path at or under the worktree is a pathspec; git says the same
        // thing about every other absolute path.
        let rest = normalized
            .strip_prefix(&root)
            .filter(|rest| rest.is_empty() || rest.starts_with('/'))
            .ok_or_else(|| path.to_string())?;
        rest.trim_start_matches('/').to_string()
    } else {
        normalize_pathspec(&format!("{prefix}{path}"))
    };

    // A `..` that survives normalization climbed past the root, which is the
    // failure `prefix_path_gently()` turns into a NULL.
    if normalized == ".." || normalized.starts_with("../") {
        return Err(path.to_string());
    }
    Ok(format!("{magic}{normalized}"))
}

/// `normalize_path_copy_len()` (path.c), the pass `prefix_path()` puts every
/// command-line path through: a `.` component disappears, a `..` component pops the
/// one before it, and repeated slashes collapse into one. So `./.` reaches the
/// pathspec machinery as `.`, `src/.` as `src`, and `a/../b` as `b`.
///
/// A pathspec that carries magic (`:(icase)x`, `:/`, …) is left alone: git parses the
/// magic first and normalizes only the path that follows, and the magic forms this
/// command sees are already repo-relative.
pub(super) fn normalize_pathspec(spec: &str) -> String {
    if spec.starts_with(':') || spec.is_empty() {
        return spec.to_string();
    }
    let absolute = spec.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    // Whether the copy currently ends in '/'. git writes a separator after every
    // component one follows and never takes it back, so `sub/`, `sub//` and `sub/.`
    // all normalize to `sub/` — and `up_one` rewinds only as far as the *previous*
    // separator, so `sub/nosuch/..` does too. That trailing '/' is not cosmetic:
    // `common_prefix()` reads it to decide the directory `read_directory()` opens,
    // which is what puts `could not open directory 'sub/sub/'` on stderr.
    let mut trailing = false;
    for component in spec.split('/') {
        match component {
            "" | "." => trailing = !out.is_empty(),
            ".." => {
                // A leading `..` has nothing to pop and stays, as git keeps it for the
                // "outside repository" diagnostics further on.
                if matches!(out.last(), Some(&last) if last != "..") {
                    out.pop();
                } else {
                    out.push("..");
                }
                trailing = !out.is_empty();
            }
            other => {
                out.push(other);
                trailing = false;
            }
        }
    }
    let joined = out.join("/");
    let joined = match trailing {
        true => format!("{joined}/"),
        false => joined,
    };
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        // Everything cancelled out: the argument named the directory it was run in.
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

/// Everything `cmd_add()`'s `exit_status` has accumulated by the time it falls
/// through to the `finish:` label, plus the two reports git prints on the way out.
pub(super) struct Finish<'a> {
    /// A path this run could not read. Without `--ignore-errors` that already
    /// aborted; with it, `add_files_to_cache()`/`add_files()` return 1 and git
    /// carries it to the end. `--dry-run` does not enter into it: `add_to_index()`
    /// only consults `ADD_CACHE_PRETEND` after `index_path()` has already failed, so
    /// `git add -n --ignore-errors -A` over an unreadable file exits 1 too (verified
    /// against git 2.55.0).
    pub had_errors: bool,
    pub ignore_errors: bool,
    /// `add_files()` found gitignored paths among the pathspecs, printed its block
    /// and set `exit_status = 1` (builtin/add.c:344-352) — inside the odb
    /// transaction, so everything else the run matched is still staged.
    pub gitignored: bool,
    /// `matched_sparse_paths` (builtin/add.c:372) and `only_match_skip_worktree`
    /// (builtin/add.c:574), both handed to `advise_on_updating_sparse_paths()`.
    pub sparse_skipped: &'a std::collections::BTreeSet<BString>,
    /// `chmod_pathspec()`'s `cannot chmod` lines — each one an `error()`, i.e. -1.
    pub chmod_errors: &'a [String],
    /// `renormalize_tracked_files()` could not index an entry: also -1.
    pub index_failed: bool,
}

/// The overall exit code: git returns 1 from a real add when `--ignore-errors`
/// let it skip at least one unreadable file, and 1 whenever a matched path lay
/// outside the sparse-checkout definition; else success.
///
/// A -1 outranks both. `exit_status` is an int folded with `|=`, so a single
/// `error()` (-1, all bits set) survives every later `|= 1`, and `cmd_add()`
/// returning -1 is what the shell reports as 255.
///
/// `advise_on_updating_sparse_paths()` names every skipped path — sorted, one per
/// line, under a three-line explanation — and follows with the advice block that
/// `advice.updateSparsePath` turns off. It runs inside `add_files()`, ahead of
/// `chmod_pathspec()`, which is why the two blocks are printed in this order.
pub(super) fn finish_code(f: Finish<'_>) -> ExitCode {
    let Finish { had_errors, ignore_errors, gitignored, sparse_skipped, chmod_errors, index_failed } =
        f;
    let sparse_reported = !sparse_skipped.is_empty();
    if sparse_reported {
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
    }
    for line in chmod_errors {
        eprintln!("{line}");
    }
    if !chmod_errors.is_empty() || index_failed {
        return ExitCode::from(255);
    }
    if sparse_reported || gitignored || (ignore_errors && had_errors) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Rust renders an OS error as `<strerror> (os error N)`; git prints only the
/// `<strerror>` half. Shared by every message that quotes one.
pub(super) fn strip_os_error(s: &str) -> &str {
    match s.find(" (os error ") {
        Some(idx) => &s[..idx],
        None => s,
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
    strip_os_error(&e.to_string()).to_string()
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

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]warn-embedded-repo`.
/// Captured byte-for-byte from stock git 2.55.0's `git add --help-all`.
const USAGE_ALL: &str = r#"usage: git add [<options>] [--] <pathspec>...

    -n, --[no-]dry-run    dry run
    -v, --[no-]verbose    be verbose

    -i, --[no-]interactive
                          interactive picking
    -p, --[no-]patch      select hunks interactively
    --[no-]auto-advance   auto advance to the next file when selecting hunks interactively
    -U, --unified <n>     generate diffs with <n> lines context
    --inter-hunk-context <n>
                          show context between diff hunks up to the specified number of lines
    -e, --[no-]edit       edit current diff and apply
    -f, --[no-]force      allow adding otherwise ignored files
    -u, --[no-]update     update tracked files
    --[no-]renormalize    renormalize EOL of tracked files (implies -u)
    -N, --[no-]intent-to-add
                          record only the fact that the path will be added later
    -A, --[no-]all        add changes from all tracked and untracked files
    --[no-]ignore-removal ignore paths removed in the working tree (same as --no-all)
    --[no-]refresh        don't add, only refresh the index
    --[no-]ignore-errors  just skip files which cannot be added because of errors
    --[no-]ignore-missing check if - even missing - files are ignored in dry run
    --[no-]sparse         allow updating entries outside of the sparse-checkout cone
    --[no-]chmod (+|-)x   override the executable bit of the listed files
    --[no-]warn-embedded-repo
                          warn when adding an embedded repository
    --[no-]pathspec-from-file <file>
                          read pathspec from file
    --[no-]pathspec-file-nul
                          with --pathspec-from-file, pathspec elements are separated with NUL character

"#;

/// `-h`: `parse_options()` prints the whole table on *stdout* and still exits 129.
fn print_usage() -> Result<ExitCode> {
    print!("{USAGE}");
    Ok(ExitCode::from(129))
}

/// `--help-all`: the same renderer over `USAGE_FULL`, so the hidden entry is
/// listed too. Same stream, same 129.
fn print_usage_all() -> Result<ExitCode> {
    print!("{USAGE_ALL}");
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

#[cfg(test)]
mod pathspec_prefix_tests {
    use super::split_pathspec_magic;

    /// Every element git treats as "already rooted" — and only those — must opt
    /// out of the prefix. Getting this backwards is silent in both directions: a
    /// `:/` that gains a prefix stops matching, and a plain path that skips one
    /// matches the wrong file.
    #[test]
    fn only_top_magic_opts_out_of_the_prefix() {
        for rooted in [":/", ":/README.md", ":(top)README.md", ":(top,icase)x", ":(prefix:2)x"] {
            assert!(split_pathspec_magic(rooted).1, "{rooted} should skip the prefix");
        }
        for prefixed in ["README.md", "./x", ":!x", ":^x", ":(icase)X", ":(exclude,icase)X", ":x"] {
            assert!(!split_pathspec_magic(prefixed).1, "{prefixed} should take the prefix");
        }
    }

    /// The path has to be split off at exactly the right byte: one short and the
    /// magic is re-emitted into the path, one long and the path loses a character.
    #[test]
    fn the_path_begins_after_the_magic() {
        for (spec, path) in [
            ("README.md", "README.md"),
            (":!src/x", "src/x"),
            (":^src/x", "src/x"),
            (":(icase)X", "X"),
            (":(exclude,icase)X", "X"),
            // `parse_short_magic` steps over the `:` that terminates the mnemonics,
            // which is what lets a path start with one of them.
            ("::x", "x"),
            (":!:x", "x"),
        ] {
            let (at, _) = split_pathspec_magic(spec);
            assert_eq!(&spec[at..], path, "path of {spec}");
        }
    }

    /// `!` and `^` are the same magic, so they must split identically — `:^x`
    /// losing its alias would send `^x` to the matcher as a literal filename.
    #[test]
    fn exclude_aliases_agree() {
        assert_eq!(split_pathspec_magic(":!x"), split_pathspec_magic(":^x"));
    }

    /// A magic-only element has no path to resolve, and an unterminated long form
    /// is git's "Missing ')'" — neither may be handed a prefix on the way past.
    #[test]
    fn degenerate_elements_are_left_alone() {
        assert_eq!(split_pathspec_magic(":/").0, 2);
        let unterminated = ":(icase";
        let (at, rooted) = split_pathspec_magic(unterminated);
        assert_eq!(at, unterminated.len(), "no path to prefix");
        assert!(rooted, "an unparseable element is passed through untouched");
    }
}
