//! `git difftool--helper` — the `GIT_EXTERNAL_DIFF`-compatible launcher that
//! `git difftool` sets as its external-diff program.
//!
//! Upstream this is a POSIX shell script (`git-difftool--helper`) that sources
//! `git-mergetool--lib`. It takes **no options at all**: every argument is
//! positional, consumed in groups of seven (`path old-file old-hex old-mode
//! new-file new-hex new-mode`, the `GIT_EXTERNAL_DIFF` calling convention), and
//! its behaviour is driven entirely by environment variables:
//! `GIT_DIFFTOOL_EXTCMD`, `GIT_DIFFTOOL_DIRDIFF`, `GIT_DIFFTOOL_NO_PROMPT`,
//! `GIT_DIFFTOOL_PROMPT`, `GIT_DIFFTOOL_TRUST_EXIT_CODE`, `GIT_DIFF_TOOL`,
//! `GIT_MERGETOOL_GUI`, `GIT_DIFF_PATH_COUNTER`, `GIT_DIFF_PATH_TOTAL`.
//!
//! The script's shape is reproduced verbatim: the prologue resolves
//! `$merge_tool` (`use_ext_cmd`, else `GIT_DIFF_TOOL`, else `get_merge_tool` —
//! whose `guess_merge_tool` writes its candidate-list guidance to stderr), then
//! either the dir-diff branch runs the tool once on the two directories or the
//! `while test $# -gt 6 … shift 7` loop runs it once per path. Each iteration
//! prompts (`\nViewing (c/t): 'path'\n` then `Launch '<tool>' [Y/n]? ` on
//! **stdout**), reads the reply, resolves the tool's command with
//! `initialize_merge_tool`/`get_merge_tool_path` — *after* the prompt, exactly
//! where the script does it — and evaluates it with `LOCAL`/`REMOTE`/`MERGED`/
//! `BASE` in scope and `BASE` exported. The `status >= 126` early exit and
//! `GIT_DIFFTOOL_TRUST_EXIT_CODE` are honoured, and the child's own stdout
//! passes through unchanged.
//!
//! The one thing that bails is a **catalogue tool's `diff_cmd`**: `vimdiff`,
//! `meld` and the rest keep theirs in a `mergetools/` shell script under
//! `$(git --exec-path)`, and nothing under `src/ported` carries that database.
//! Selecting such a tool, prompting for it, and every diagnostic that precedes
//! the launch (`error: difftool.<tool>.cmd not set for tool '<tool>'`,
//! `error: unknown tool variant`, `The diff tool <tool> is not available as
//! '<path>'`) are reproduced; only reaching the launch itself is refused, since
//! exiting 0 without running anything would be indistinguishable from a
//! successful launch while leaving the diff unreviewed.
//!
//! Known divergence: a `GIT_DIFFTOOL_EXTCMD` that is itself a *shell builtin
//! misuse* (e.g. `exit 3`, which upstream's `eval` runs in the parent shell and
//! which fails with "too many arguments" once the two path arguments are
//! appended) is executed here in a child `sh`, so its diagnostic text and
//! resulting status come from that child instead of the parent script's shell.

use anyhow::Result;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use super::difftool::{read_reply, select_tool, should_prompt, Launched, Tool};

/// One `GIT_EXTERNAL_DIFF` argument group is seven positionals; the script's
/// loop condition is `test $# -gt 6`.
const GROUP: usize = 7;

/// `git difftool--helper` — launch a diff tool for each path group.
#[allow(non_snake_case)] // maps to git's `difftool--helper` subcommand
pub fn difftool__helper(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. The helper's positionals are paths,
    // so strip exactly one leading literal verb.
    let args = match args.first().map(String::as_str) {
        Some("difftool--helper") => &args[1..],
        _ => args,
    };

    let dirdiff = env_nonempty("GIT_DIFFTOOL_DIRDIFF").is_some();
    let trust = std::env::var("GIT_DIFFTOOL_TRUST_EXIT_CODE").as_deref() == Ok("true");

    // The helper also runs outside a repository, where `git config` still reads
    // the global, system and `GIT_CONFIG_*` files.
    let repo = gix::discover(".").ok();
    let snapshot = repo.as_ref().map(|r| r.config_snapshot());
    let config = match &snapshot {
        Some(s) => s.plumbing().clone(),
        None => gix::config::File::from_globals()?,
    };

    // The script's prologue, which runs before either branch.
    let tool = select_tool(
        &config,
        env_nonempty("GIT_DIFFTOOL_EXTCMD").as_deref(),
        env_nonempty("GIT_DIFF_TOOL").as_deref(),
        gui_override(),
    )?;

    if dirdiff {
        // `LOCAL="$1"; REMOTE="$2"; initialize_merge_tool … run_merge_tool "$merge_tool" false`
        // — `use_ext_cmd` is never consulted here, so an extcmd run reaches this
        // branch with whatever `get_merge_tool` picked.
        let (Some(local), Some(remote)) = (args.first(), args.get(1)) else {
            return Ok(ExitCode::SUCCESS);
        };
        let status =
            match super::difftool::launch_dir_tool(&config, &tool, Path::new(local), Path::new(remote))? {
                // `initialize_merge_tool "$merge_tool" || exit 1`.
                Launched::HelperExit(code) => return Ok(ExitCode::from(code as u8)),
                Launched::Status(status) => status,
            };
        if status >= 126 || trust {
            return Ok(ExitCode::from(status as u8));
        }
        return Ok(ExitCode::SUCCESS);
    }

    let prompt = should_prompt(None, Some(&config));
    let counter = std::env::var("GIT_DIFF_PATH_COUNTER").unwrap_or_default();
    let total = std::env::var("GIT_DIFF_PATH_TOTAL").unwrap_or_default();

    let mut i = 0;
    while args.len() - i > GROUP - 1 {
        // MERGED/BASE = the work-tree path, LOCAL = a/, REMOTE = b/.
        let merged = &args[i];
        let local = &args[i + 1];
        let remote = &args[i + 4];

        let status = match launch_merge_tool(
            &config, &tool, merged, local, remote, prompt, &counter, &total,
        )? {
            // `initialize_merge_tool "$merge_tool" || exit 1` leaves the loop at
            // once, without the `shift 7` that would move to the next path.
            Launched::HelperExit(code) => return Ok(ExitCode::from(code as u8)),
            Launched::Status(status) => status,
        };

        // Command not found (127), not executable (126) or death by signal.
        if status >= 126 {
            return Ok(ExitCode::from(status as u8));
        }
        if status != 0 && trust {
            return Ok(ExitCode::from(status as u8));
        }
        i += GROUP;
    }

    Ok(ExitCode::SUCCESS)
}

/// The script's `launch_merge_tool`, returning the status it would record in `$?`.
#[allow(clippy::too_many_arguments)]
fn launch_merge_tool(
    config: &gix::config::File,
    tool: &Tool,
    merged: &str,
    local: &str,
    remote: &str,
    prompt: bool,
    counter: &str,
    total: &str,
) -> Result<Launched> {
    if prompt {
        print!(
            "\nViewing ({counter}/{total}): '{merged}'\nLaunch '{}' [Y/n]? ",
            tool.label()
        );
        std::io::stdout().flush()?;

        match read_reply()? {
            // `read ans || return` — at end of input the function returns the
            // failing read's status without launching anything.
            None => return Ok(Launched::Status(1)),
            // `test "$ans" = n` — a bare `return` after a true `test` is 0.
            Some(ans) if ans == "n" => return Ok(Launched::Status(0)),
            Some(_) => {}
        }
    }

    super::difftool::launch_file_tool(config, tool, Path::new(local), Path::new(remote), merged)
}

/// `gui_mode`'s `GIT_MERGETOOL_GUI` override, which `git difftool` exports only
/// for an explicit `--gui`/`--no-gui`. Unset leaves `difftool.guiDefault` to
/// decide.
fn gui_override() -> Option<bool> {
    match std::env::var("GIT_MERGETOOL_GUI").ok()?.as_str() {
        "true" => Some(true),
        "" => None,
        _ => Some(false),
    }
}

/// An environment variable, treating unset and empty alike — the script tests
/// these with `test -n`.
fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}
