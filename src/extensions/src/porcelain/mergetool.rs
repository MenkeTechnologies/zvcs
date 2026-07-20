//! `git mergetool` — run merge conflict resolution tools to resolve conflicts.
//!
//! Stock `git mergetool` is a POSIX shell script (`git-mergetool.sh`) layered on
//! `git-mergetool--lib` and the `mergetools/` backend catalogue. Its actual work —
//! picking a backend, materialising index stages 1/2/3 into `BASE`/`LOCAL`/`REMOTE`
//! temp files, exec'ing an external (usually graphical) program, prompting on the
//! terminal, then judging success by exit code or mtime — is shell and process
//! orchestration around third-party binaries. None of that substrate is in the
//! vendored gitoxide crates, and faking it would silently corrupt conflicted files.
//!
//! Ported here, byte-faithfully, is everything the script does *before* it touches
//! a tool:
//!   * the option loop of `main()`, including its quirks (`--tool*` prefix match,
//!     the stuck `=` form, `-t` with a missing value falling through to `usage`);
//!   * `git-sh-setup`'s leading-`-h` handling — usage to stdout, exit 0, and it
//!     works outside a repository;
//!   * `usage` — the same one-line usage to stderr, exit 1;
//!   * `require_work_tree`;
//!   * the full "is there anything to do" decision, i.e. the `MERGE_RR` /
//!     `git rerere remaining` branch and the `diff --diff-filter=U` pathspec
//!     filter, ending in `print_noop_and_exit`'s `No files need merging` / exit 0.
//!
//! When that decision yields at least one conflicted path — the point where the
//! script would print `Merging:` and start invoking a backend — this bails instead
//! of emitting partial output. `--tool-help` bails likewise: its listing is a probe
//! of the installed `mergetools/` scripts against `$PATH`, which does not exist here.

use anyhow::{bail, Result};
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};

/// `$USAGE` from `git-mergetool.sh`, rendered by `git-sh-setup`'s `usage`/`-h`
/// handling as `usage: <dashless> <USAGE>`.
const USAGE: &str = "usage: git mergetool [--tool=tool] [--tool-help] [-y|--no-prompt|--prompt] [-g|--gui|--no-gui] [-O<orderfile>] [file to merge] ...";

/// `git mergetool` — see the module docs for exactly what is and is not ported.
///
/// Options parsed exactly as the script does: `-t <tool>` / `--tool=<tool>`,
/// `-y` / `--no-prompt`, `--prompt`, `-g` / `--gui`, `--no-gui`, `-O<orderfile>`,
/// `--`, and trailing pathspecs. The tool-selection, prompt and order options only
/// steer the backend-invocation loop, which is unported, so they are accepted and
/// have no effect on the paths that are — matching stock git, which also ignores
/// them entirely when nothing needs merging.
///
/// Exit codes: 0 for `-h` and for `No files need merging`, 1 for a usage error.
pub fn mergetool(args: &[String]) -> Result<ExitCode> {
    let rest = args.get(1..).unwrap_or(&[]);

    // `git-sh-setup` inspects `$1` only, before any repository setup, and the
    // script sets NONGIT_OK=Yes — so this works outside a repository too.
    if rest.first().map(String::as_str) == Some("-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }

    // Port of main()'s `while test $# != 0` option loop, case arms in order.
    let mut i = 0usize;
    while i < rest.len() {
        let a = rest[i].as_str();
        if a.starts_with("--tool-help") {
            // `--tool-help` and `--tool-help=<mode>` both list backends and exit.
            bail!(
                "--tool-help lists the mergetools/ shell backends git ships and probes each \
                 against $PATH; that catalogue is not part of the vendored gitoxide crates"
            );
        } else if a == "-t" || a.starts_with("--tool") {
            // `case "$#,$1" in *,*=*) stuck ;; 1,*) usage ;; *) take $2 ;; esac`
            if !a.contains('=') {
                if i + 1 >= rest.len() {
                    return Ok(usage_error());
                }
                i += 1;
            }
        } else if matches!(a, "--no-gui" | "-g" | "--gui" | "-y" | "--no-prompt" | "--prompt") {
            // Only steer the unported backend-invocation loop.
        } else if a.starts_with("-O") {
            // Orders the unported backend-invocation loop; never observable here.
        } else if a == "--" {
            i += 1;
            break;
        } else if a.starts_with('-') {
            // The script's `-*` catch-all, which a bare `-` also reaches.
            return Ok(usage_error());
        } else {
            break;
        }
        i += 1;
    }
    let pathspecs = &rest[i..];

    let repo = gix::discover(".")?;
    // `require_work_tree`. git's wording embeds the script's own absolute path,
    // which cannot be reproduced, so this states the condition instead.
    if repo.workdir().is_none() {
        bail!("this operation must be run in a work tree");
    }

    // `git diff --name-only --diff-filter=U` — the unmerged index paths.
    let unmerged = unmerged_paths(&repo)?;

    // With no pathspecs and a `MERGE_RR` present, the script narrows the candidate
    // set to `git rerere remaining` before the diff. Every path that survives both
    // is unmerged *and* remaining, so the two filters compose to one intersection;
    // when rerere is disabled `remaining` prints nothing and the script no-ops.
    let files: Vec<BString> = if pathspecs.is_empty() && repo.git_dir().join("MERGE_RR").exists() {
        let remaining = rerere_remaining(&repo, &unmerged)?;
        unmerged
            .into_iter()
            .filter(|p| remaining.iter().any(|r| r == p))
            .collect()
    } else if pathspecs.is_empty() {
        unmerged
    } else {
        let specs = resolve_pathspecs(&repo, pathspecs)?;
        unmerged
            .into_iter()
            .filter(|p| specs.iter().any(|s| pathspec_matches(s, p)))
            .collect()
    };

    if files.is_empty() {
        // `print_noop_and_exit`.
        println!("No files need merging");
        return Ok(ExitCode::SUCCESS);
    }

    bail!(
        "resolving {} conflicted path(s) needs git's mergetools/ shell backends, the \
         BASE/LOCAL/REMOTE temp-file staging around them, and the interactive prompt loop; \
         that substrate is not in the vendored gitoxide crates (ported: option parsing, -h, \
         and the 'No files need merging' no-op path)",
        files.len()
    );
}

/// `usage` from `git-sh-setup` with an empty `OPTIONS_SPEC`: `die` the one-line
/// usage on stderr, exit 1.
fn usage_error() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(1)
}

/// The unmerged paths of the index, deduplicated, in index (path-sorted) order —
/// what `git diff --name-only --diff-filter=U` reports.
fn unmerged_paths(repo: &gix::Repository) -> Result<Vec<BString>> {
    let index = repo.open_index()?;
    let mut out: Vec<BString> = Vec::new();
    for entry in index.entries() {
        if entry.stage_raw() == 0 {
            continue;
        }
        let path = entry.path(&index);
        if out.last().map(|p| p.as_bstr()) != Some(path) {
            out.push(path.to_owned());
        }
    }
    Ok(out)
}

/// The `git rerere remaining` output, restricted to what the caller can act on.
///
/// `rerere_remaining()` is `MERGE_RR`'s recorded paths plus the conflicts rerere
/// cannot track ("punted" ones), minus everything the index shows resolved. Since
/// the caller intersects the result with the unmerged paths, the resolved
/// subtraction is a no-op here and only those two additions are computed. An
/// explicit `rerere.enabled=false` (or no `rr-cache` when the setting is unset)
/// makes `git rerere remaining` print nothing at all.
fn rerere_remaining(repo: &gix::Repository, unmerged: &[BString]) -> Result<Vec<BString>> {
    if !is_rerere_enabled(repo) {
        return Ok(Vec::new());
    }

    let mut out = read_merge_rr(repo)?;

    // `check_one_conflict()`: only a plain stage #2 + stage #3 pair of regular
    // files is recordable; every other unmerged shape is always reported.
    let index = repo.open_index()?;
    let cache = index.entries();
    let mut i = 0usize;
    while i < cache.len() {
        let name = cache[i].path(&index).to_owned();
        if cache[i].stage_raw() == 0 {
            i += 1;
            continue;
        }

        let mut j = i;
        while j < cache.len() && cache[j].stage_raw() == 1 {
            j += 1;
        }
        let three_staged = j + 1 < cache.len()
            && cache[j].stage_raw() == 2
            && cache[j + 1].stage_raw() == 3
            && cache[j + 1].path(&index) == cache[j].path(&index)
            && is_regular_file(cache[j].mode)
            && is_regular_file(cache[j + 1].mode);
        if !three_staged && !out.contains(&name) {
            out.push(name.clone());
        }

        while j < cache.len() && cache[j].path(&index) == name {
            j += 1;
        }
        i = j;
    }

    // Recorded paths that no longer exist as index entries cannot match anything
    // the caller holds; dropping them keeps the intersection cheap.
    out.retain(|p| unmerged.iter().any(|u| u == p));
    Ok(out)
}

/// `is_rerere_enabled()`: an explicit `rerere.enabled=false` disables it, unset
/// means "enabled only if `rr-cache` already exists", true enables it.
fn is_rerere_enabled(repo: &gix::Repository) -> bool {
    match repo.config_snapshot().boolean("rerere.enabled") {
        Some(false) => false,
        Some(true) => true,
        None => repo.common_dir().join("rr-cache").is_dir(),
    }
}

/// `read_rr()`, reduced to the worktree paths: records are
/// `<hex>[.<variant>]\t<path>\0`.
fn read_merge_rr(repo: &gix::Repository) -> Result<Vec<BString>> {
    let hexsz = repo.object_hash().len_in_hex();
    let Ok(data) = std::fs::read(repo.git_dir().join("MERGE_RR")) else {
        return Ok(Vec::new());
    };

    let mut out: Vec<BString> = Vec::new();
    for rec in data.split(|&b| b == 0) {
        if rec.is_empty() {
            continue;
        }
        if rec.len() < hexsz + 2 {
            bail!("corrupt MERGE_RR");
        }
        let Some(tab_at) = rec.iter().position(|&b| b == b'\t') else {
            bail!("corrupt MERGE_RR");
        };
        let path = BString::from(&rec[tab_at + 1..]);
        if !out.contains(&path) {
            out.push(path);
        }
    }
    Ok(out)
}

/// `S_ISREG()` on an index entry mode — symlinks and gitlinks are excluded.
fn is_regular_file(mode: gix::index::entry::Mode) -> bool {
    mode.bits() & 0o170000 == 0o100000
}

/// Qualify the command-line pathspecs against the current prefix, the way
/// `git rev-parse --sq --prefix "$prefix" -- "$@"` does before the diff.
///
/// Only literal pathspecs are ported: git's magic (`:(glob)`, `:!`, …) and glob
/// characters select a different match function, and guessing wrong would report
/// `No files need merging` over a real conflict.
fn resolve_pathspecs(repo: &gix::Repository, specs: &[String]) -> Result<Vec<BString>> {
    let prefix = repo
        .prefix()?
        .map(|p| p.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"))
        .unwrap_or_default();

    let mut out = Vec::new();
    for spec in specs {
        if spec.starts_with(':') || spec.contains(['*', '?', '[']) {
            bail!("pathspec magic and glob pathspecs ({spec:?}) are not supported");
        }
        if spec.starts_with('/') || spec.split('/').any(|c| c == "..") {
            bail!("pathspecs outside the current directory ({spec:?}) are not supported");
        }
        let joined = match (prefix.is_empty(), spec.is_empty()) {
            (true, _) => spec.clone(),
            (false, true) => prefix.clone(),
            (false, false) => format!("{prefix}/{spec}"),
        };
        out.push(BString::from(joined.trim_end_matches('/').to_owned()));
    }
    Ok(out)
}

/// git's literal pathspec match: an exact hit, a directory prefix, or the
/// empty pathspec (which matches everything).
fn pathspec_matches(spec: &BString, path: &BString) -> bool {
    if spec.is_empty() {
        return true;
    }
    if spec == path {
        return true;
    }
    path.len() > spec.len() && path.starts_with(spec.as_slice()) && path[spec.len()] == b'/'
}
