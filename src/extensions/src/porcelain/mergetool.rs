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
//!     filter, ending in `print_noop_and_exit`'s `No files need merging` / exit 0;
//!   * `show_tool_help`, i.e. all of `--tool-help[=<mode>]` — see [`show_tool_help`]
//!     and [`TOOLS`] for how the backend catalogue it lists is represented here.
//!
//! When that decision yields at least one conflicted path — the point where the
//! script would print `Merging:` and start invoking a backend — this bails instead
//! of emitting partial output.

use anyhow::{bail, Result};
use std::path::Path;
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
        if let Some(mode) = a.strip_prefix("--tool-help=") {
            // `TOOL_MODE=${1#--tool-help=}; show_tool_help` — the arm runs before
            // `git_dir_init`/`require_work_tree` and ends in `exit 0`.
            show_tool_help(mode);
            return Ok(ExitCode::SUCCESS);
        } else if a == "--tool-help" {
            // `$TOOL_MODE` is still its `git-mergetool` default of `merge`.
            show_tool_help("merge");
            return Ok(ExitCode::SUCCESS);
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

/// One mode's view of a backend: what `is_available` probes and what the listing prints.
struct Backend {
    /// `translate_merge_tool_path <tool>` — the name looked up on `$PATH`. It is
    /// often not the tool name (`vscode` probes `code`, `araxis` probes `compare`).
    path: &'static str,
    /// `merge_cmd_help <tool>` / `diff_cmd_help <tool>`.
    help: &'static str,
}

/// A single entry of the variant list `show_tool_names` iterates.
struct Tool {
    name: &'static str,
    /// The `mergetools/` scripts whose `list_tool_variants` emits this name, for
    /// names that are not themselves script names. Empty marks a script name.
    ///
    /// `setup_user_tool` replaces `list_tool_variants` with one that echoes only
    /// the script's own name, so a configured `mergetool.<script>.cmd` stops that
    /// script from contributing its extra variants. A variant survives while any
    /// one of its producers is unconfigured — which is why `vimdiff1` still shows
    /// up when only `mergetool.vimdiff.cmd` is set, `gvimdiff`/`nvimdiff` (both of
    /// which source `mergetools/vimdiff`) still emitting the full list.
    producers: &'static [&'static str],
    /// `None` where `can_merge` is false, or where the merge-mode
    /// `list_tool_variants` never emits this name.
    merge: Option<Backend>,
    /// `None` where `can_diff` is false, or where the diff-mode
    /// `list_tool_variants` never emits this name — diff mode has no numbered
    /// `vimdiff` variants.
    diff: Option<Backend>,
}

const fn backend(path: &'static str, help: &'static str) -> Option<Backend> {
    Some(Backend { path, help })
}

/// The three scripts that all source `mergetools/vimdiff` and so all emit the
/// whole `[g|n]vimdiff[1-3]` family.
const VIM_FAMILY: &[&str] = &["gvimdiff", "nvimdiff", "vimdiff"];

/// The `mergetools/` catalogue, in the `sort -u` order `show_tool_names` iterates.
///
/// git derives this by sourcing every script in `$(git --exec-path)/mergetools`
/// and calling `list_tool_variants`, `can_merge`/`can_diff`, `merge_cmd_help`/
/// `diff_cmd_help` and `translate_merge_tool_path` on each. Those are arbitrary
/// shell, so the *values* they yield are transcribed here as data — read out of
/// git 2.55.0's scripts — while the enumeration, `$PATH` probe, config merge,
/// sort and formatting around them are implemented below. A git whose catalogue
/// differs from 2.55.0's will therefore list a different set than this does.
const TOOLS: &[Tool] = &[
    Tool { name: "araxis", producers: &[], merge: backend("compare", "Use Araxis Merge (requires a graphical session)"), diff: backend("compare", "Use Araxis Merge (requires a graphical session)") },
    Tool { name: "bc", producers: &[], merge: backend("bcompare", "Use Beyond Compare (requires a graphical session)"), diff: backend("bcompare", "Use Beyond Compare (requires a graphical session)") },
    Tool { name: "bc3", producers: &["bc"], merge: backend("bcompare", "Use Beyond Compare (requires a graphical session)"), diff: backend("bcompare", "Use Beyond Compare (requires a graphical session)") },
    Tool { name: "bc4", producers: &["bc"], merge: backend("bcompare", "Use Beyond Compare (requires a graphical session)"), diff: backend("bcompare", "Use Beyond Compare (requires a graphical session)") },
    // `translate_merge_tool_path` is the one that branches on the mode.
    Tool { name: "codecompare", producers: &[], merge: backend("CodeMerge", "Use Code Compare (requires a graphical session)"), diff: backend("CodeCompare", "Use Code Compare (requires a graphical session)") },
    Tool { name: "deltawalker", producers: &[], merge: backend("DeltaWalker", "Use DeltaWalker (requires a graphical session)"), diff: backend("DeltaWalker", "Use DeltaWalker (requires a graphical session)") },
    Tool { name: "diffmerge", producers: &[], merge: backend("diffmerge", "Use DiffMerge (requires a graphical session)"), diff: backend("diffmerge", "Use DiffMerge (requires a graphical session)") },
    Tool { name: "diffuse", producers: &[], merge: backend("diffuse", "Use Diffuse (requires a graphical session)"), diff: backend("diffuse", "Use Diffuse (requires a graphical session)") },
    Tool { name: "ecmerge", producers: &[], merge: backend("ecmerge", "Use ECMerge (requires a graphical session)"), diff: backend("ecmerge", "Use ECMerge (requires a graphical session)") },
    Tool { name: "emerge", producers: &[], merge: backend("emacs", "Use Emacs' Emerge"), diff: backend("emacs", "Use Emacs' Emerge") },
    Tool { name: "examdiff", producers: &[], merge: backend("ExamDiff.com", "Use ExamDiff Pro (requires a graphical session)"), diff: backend("ExamDiff.com", "Use ExamDiff Pro (requires a graphical session)") },
    Tool { name: "guiffy", producers: &[], merge: backend("guiffy", "Use Guiffy's Diff Tool (requires a graphical session)"), diff: backend("guiffy", "Use Guiffy's Diff Tool (requires a graphical session)") },
    Tool { name: "gvimdiff", producers: &[], merge: backend("gvim", "Use gVim (requires a graphical session) with a custom layout (see `git help mergetool`'s `BACKEND SPECIFIC HINTS` section)"), diff: backend("gvim", "Use gVim (requires a graphical session)") },
    Tool { name: "gvimdiff1", producers: VIM_FAMILY, merge: backend("gvim", "Use gVim (requires a graphical session) with a 2 panes layout (LOCAL and REMOTE)"), diff: None },
    Tool { name: "gvimdiff2", producers: VIM_FAMILY, merge: backend("gvim", "Use gVim (requires a graphical session) with a 3 panes layout (LOCAL, MERGED and REMOTE)"), diff: None },
    Tool { name: "gvimdiff3", producers: VIM_FAMILY, merge: backend("gvim", "Use gVim (requires a graphical session) where only the MERGED file is shown"), diff: None },
    Tool { name: "kdiff3", producers: &[], merge: backend("kdiff3.exe", "Use KDiff3 (requires a graphical session)"), diff: backend("kdiff3.exe", "Use KDiff3 (requires a graphical session)") },
    // `can_merge` returns 1: kompare is diff-only.
    Tool { name: "kompare", producers: &[], merge: None, diff: backend("kompare", "Use Kompare (requires a graphical session)") },
    Tool { name: "meld", producers: &[], merge: backend("meld", "Use Meld (requires a graphical session) with optional `auto merge` (see `git help mergetool`'s `CONFIGURATION` section)"), diff: backend("meld", "Use Meld (requires a graphical session)") },
    Tool { name: "nvimdiff", producers: &[], merge: backend("nvim", "Use Neovim with a custom layout (see `git help mergetool`'s `BACKEND SPECIFIC HINTS` section)"), diff: backend("nvim", "Use Neovim") },
    Tool { name: "nvimdiff1", producers: VIM_FAMILY, merge: backend("nvim", "Use Neovim with a 2 panes layout (LOCAL and REMOTE)"), diff: None },
    Tool { name: "nvimdiff2", producers: VIM_FAMILY, merge: backend("nvim", "Use Neovim with a 3 panes layout (LOCAL, MERGED and REMOTE)"), diff: None },
    Tool { name: "nvimdiff3", producers: VIM_FAMILY, merge: backend("nvim", "Use Neovim where only the MERGED file is shown"), diff: None },
    Tool { name: "opendiff", producers: &[], merge: backend("opendiff", "Use FileMerge (requires a graphical session)"), diff: backend("opendiff", "Use FileMerge (requires a graphical session)") },
    Tool { name: "p4merge", producers: &[], merge: backend("p4merge", "Use HelixCore P4Merge (requires a graphical session)"), diff: backend("p4merge", "Use HelixCore P4Merge (requires a graphical session)") },
    Tool { name: "smerge", producers: &[], merge: backend("smerge", "Use Sublime Merge (requires a graphical session)"), diff: backend("smerge", "Use Sublime Merge (requires a graphical session)") },
    Tool { name: "tkdiff", producers: &[], merge: backend("tkdiff", "Use TkDiff (requires a graphical session)"), diff: backend("tkdiff", "Use TkDiff (requires a graphical session)") },
    // `can_diff` returns 1: tortoisemerge is merge-only.
    Tool { name: "tortoisemerge", producers: &[], merge: backend("tortoisemerge", "Use TortoiseMerge (requires a graphical session)"), diff: None },
    Tool { name: "vimdiff", producers: &[], merge: backend("vim", "Use Vim with a custom layout (see `git help mergetool`'s `BACKEND SPECIFIC HINTS` section)"), diff: backend("vim", "Use Vim") },
    Tool { name: "vimdiff1", producers: VIM_FAMILY, merge: backend("vim", "Use Vim with a 2 panes layout (LOCAL and REMOTE)"), diff: None },
    Tool { name: "vimdiff2", producers: VIM_FAMILY, merge: backend("vim", "Use Vim with a 3 panes layout (LOCAL, MERGED and REMOTE)"), diff: None },
    Tool { name: "vimdiff3", producers: VIM_FAMILY, merge: backend("vim", "Use Vim where only the MERGED file is shown"), diff: None },
    Tool { name: "vscode", producers: &[], merge: backend("code", "Use Visual Studio Code (requires a graphical session)"), diff: backend("code", "Use Visual Studio Code (requires a graphical session)") },
    Tool { name: "winmerge", producers: &[], merge: backend("WinMergeU.exe", "Use WinMerge (requires a graphical session)"), diff: backend("WinMergeU.exe", "Use WinMerge (requires a graphical session)") },
    Tool { name: "xxdiff", producers: &[], merge: backend("xxdiff", "Use xxdiff (requires a graphical session)"), diff: backend("xxdiff", "Use xxdiff (requires a graphical session)") },
];

/// `show_tool_help` from `git-mergetool--lib`: the available backends, the
/// `user-defined:` block from `<mode>tool.*.cmd`, the unavailable backends, and
/// the closing windowed-environment note — all on stdout, then `exit 0`.
///
/// `mode` is `$TOOL_MODE`, which `--tool-help=<mode>` sets to an arbitrary string.
/// Anything other than `merge` or `diff` makes `mode_ok` false for every backend,
/// so only the config-derived block survives.
fn show_tool_help(mode: &str) {
    let tool_opt = format!("'git {mode}tool --tool=<tool>'");

    // `git config --get-regexp` reads global and system config too, and this arm
    // runs before any repository setup — so fall back to the globals outside one.
    let config = match gix::discover(".") {
        Ok(repo) => Some(repo.config_snapshot().plumbing().clone()),
        Err(_) => gix::config::File::from_globals().ok(),
    };

    let mut config_tools = Vec::new();
    // `get_merge_tool_cmd`: the names whose `.cmd` is set to something non-empty.
    let mut configured = Vec::new();
    if let Some(config) = &config {
        if mode == "diff" {
            list_config_tools(config, "difftool", &mut config_tools, &mut configured);
        }
        list_config_tools(config, "mergetool", &mut config_tools, &mut configured);
    }
    // `{ ... } | sort`. The `\t\t` prefix is on every line, so this orders by
    // tool name; git's `sort` is locale-sensitive where this is byte-wise.
    config_tools.sort();
    let extra_content = if config_tools.is_empty() {
        String::new()
    } else {
        format!("\tuser-defined:\n{}", config_tools.join("\n"))
    };

    // A configured `.cmd` makes that script emit only its own name, so a variant
    // survives only while at least one of its producers is unconfigured.
    let listed: Vec<(&str, &Backend)> = TOOLS
        .iter()
        .filter(|t| {
            t.producers.is_empty()
                || t.producers
                    .iter()
                    .any(|s| !configured.iter().any(|c| c.as_str() == *s))
        })
        .filter_map(|t| {
            let b = match mode {
                "diff" => t.diff.as_ref(),
                "merge" => t.merge.as_ref(),
                _ => None,
            };
            b.map(|b| (t.name, b))
        })
        .collect();

    let (available, unavailable): (Vec<_>, Vec<_>) =
        listed.into_iter().partition(|(_, b)| is_available(b.path));

    let mut any_shown = show_tool_names(
        &available,
        Some(&format!("{tool_opt} may be set to one of the following:")),
        &format!("No suitable tool for 'git {mode}tool --tool=<tool>' found."),
        &extra_content,
    );
    any_shown |= show_tool_names(
        &unavailable,
        Some("\nThe following tools are valid, but not currently available:"),
        "",
        "",
    );

    if any_shown {
        println!();
        println!("Some of the tools listed above only work in a windowed");
        println!("environment. If run in a terminal-only session, they will fail.");
    }
}

/// `show_tool_names`: print `preamble` lazily before the first line it has to
/// emit, then one padded line per tool, then `extra_content`, or `not_found_msg`
/// if it turned out there was nothing at all. Returns `shown_any`.
fn show_tool_names(
    tools: &[(&str, &Backend)],
    preamble: Option<&str>,
    not_found_msg: &str,
    extra_content: &str,
) -> bool {
    let mut preamble = preamble.filter(|p| !p.is_empty());
    let mut shown_any = false;

    for (name, b) in tools {
        if let Some(p) = preamble.take() {
            println!("{p}");
        }
        shown_any = true;
        // `printf "%s%-15s  %s\n"`. The help text is one word because
        // `git-mergetool--lib` sets `IFS` to a lone linefeed.
        println!("\t\t{name:<15}  {}", b.help);
    }

    if !extra_content.is_empty() {
        // No newline after the preamble here: git avoids a blank line when the
        // config block is the first thing shown.
        if let Some(p) = preamble.take() {
            print!("{p}");
        }
        shown_any = true;
        print!("\n{extra_content}\n");
    }

    if preamble.is_some() && !not_found_msg.is_empty() {
        println!("{not_found_msg}");
    }
    shown_any
}

/// `list_config_tools`: one `\t\t`-prefixed line per `<section>.<tool>.cmd`.
///
/// git reads these from `git config --get-regexp <section>'\..*\.cmd'`, which
/// prints `<key> <value>`, and then splits with `read -r key value` under the
/// `IFS=<LF>` set at the top of `git-mergetool--lib`. Nothing splits on the
/// space, so the whole line lands in `key`, and stripping `<section>.` off the
/// front and `.cmd` off the *end* leaves the value dangling in the output. That
/// is what git prints, so it is what this reproduces.
///
/// `configured` collects the subsection names carrying a non-empty `cmd`, which is
/// what `get_merge_tool_cmd` tests before `setup_user_tool` narrows the variants.
fn list_config_tools(
    config: &gix::config::File,
    section: &str,
    out: &mut Vec<String>,
    configured: &mut Vec<String>,
) {
    let Some(sections) = config.sections_by_name(section) else {
        return;
    };
    for s in sections {
        let Some(sub) = s.header().subsection_name() else {
            continue;
        };
        let sub = sub.to_str_lossy();
        for value in s.values("cmd") {
            let line = format!("{sub}.cmd {}", value.to_str_lossy());
            out.push(format!("\t\t{}", line.strip_suffix(".cmd").unwrap_or(&line)));
            if !value.is_empty() && !configured.iter().any(|c| c.as_str() == &*sub) {
                configured.push(sub.to_string());
            }
        }
    }
}

/// `is_available`: `type "$merge_tool_path"`, i.e. an executable of that name on
/// `$PATH`. None of the catalogue's names are shell builtins, so the builtin and
/// function lookups `type` also does cannot match.
fn is_available(path: &str) -> bool {
    if path.contains('/') {
        return is_executable(Path::new(path));
    }
    let Some(var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&var).any(|dir| {
        // POSIX: an empty `$PATH` element means the current directory.
        let dir = if dir.as_os_str().is_empty() { ".".into() } else { dir };
        is_executable(&dir.join(path))
    })
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
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
