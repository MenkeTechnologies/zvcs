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
//! `GIT_DIFF_PATH_COUNTER`, `GIT_DIFF_PATH_TOTAL`.
//!
//! What is ported (checked against git 2.55.0):
//!
//!   * The **`--extcmd` path** — `GIT_DIFFTOOL_EXTCMD` set, no
//!     `GIT_DIFFTOOL_DIRDIFF`. This is a faithful port of the script: the
//!     `while test $# -gt 6 … shift 7` loop, the prompt (`\nViewing (c/t):
//!     'path'\n` then `Launch 'cmd' [Y/n]? ` on **stdout**), the `read ans`
//!     reply handling, `eval $GIT_DIFFTOOL_EXTCMD '"$LOCAL"' '"$REMOTE"'` with
//!     `MERGED`/`BASE` also in scope and `BASE` exported, the `status >= 126`
//!     early exit, and `GIT_DIFFTOOL_TRUST_EXIT_CODE`. The child command's own
//!     stdout passes through unchanged, so this is byte-identical.
//!   * `GIT_DIFFTOOL_DIRDIFF` **with** `GIT_DIFFTOOL_EXTCMD`: the script never
//!     consults the extcmd in dir-diff mode, so it runs
//!     `initialize_merge_tool ""`, which fails with ``error: difftool..cmd not
//!     set for tool ''`` on stderr and exit 1. Reproduced.
//!   * `GIT_DIFF_TOOL` set with fewer than seven arguments and no dir-diff: no
//!     tool resolution and no loop iteration, so the script exits 0 silently.
//!
//! What bails, and why — the missing substrate is the **tool database**:
//! `get_merge_tool` / `initialize_merge_tool` / `run_merge_tool` live in
//! `git-mergetool--lib` and source one script per tool out of
//! `$(git --exec-path)/mergetools/`, each supplying that tool's `diff_cmd`,
//! availability probe and exit-code trustworthiness. Nothing under
//! `src/ported/gix*` carries any of it. Concretely:
//!
//!   * No `GIT_DIFFTOOL_EXTCMD` and no `GIT_DIFF_TOOL` → `get_merge_tool` must
//!     run, walking `diff.tool`/`merge.tool` and then *guessing* from the
//!     installed tool list; it can exit non-zero with a multi-line message
//!     naming the tools present on the machine. Not derivable here, even for a
//!     short argument list where the loop would not run.
//!   * `GIT_DIFF_TOOL` (or a configured tool) with a full seven-argument group,
//!     or dir-diff without an extcmd → `run_merge_tool` must build and execute
//!     that tool's `diff_cmd`.
//!
//! Those paths are not approximated: exiting 0 without launching anything would
//! be indistinguishable from a successful launch while leaving the diff
//! unreviewed.
//!
//! Known divergence: a `GIT_DIFFTOOL_EXTCMD` that is itself a *shell builtin
//! misuse* (e.g. `exit 3`, which upstream's `eval` runs in the parent shell and
//! which fails with "too many arguments" once the two path arguments are
//! appended) is executed here in a child `sh`, so its diagnostic text and
//! resulting status come from that child instead of the parent script's shell.

use anyhow::{Result, bail};
use std::io::{Read, Write};
use std::process::{Command, ExitCode, ExitStatus, Stdio};

/// One `GIT_EXTERNAL_DIFF` argument group is seven positionals; the script's
/// loop condition is `test $# -gt 6`.
const GROUP: usize = 7;

/// `git difftool--helper` — launch a diff tool for each path group.
///
/// See the module documentation for the invocations that are reproduced
/// byte-for-byte and for the tool database the rest would need.
pub fn difftool__helper(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. The helper's positionals are paths,
    // so strip exactly one leading literal verb.
    let args = match args.first().map(String::as_str) {
        Some("difftool--helper") => &args[1..],
        _ => args,
    };

    let extcmd = env_nonempty("GIT_DIFFTOOL_EXTCMD");
    let dirdiff = env_nonempty("GIT_DIFFTOOL_DIRDIFF").is_some();
    let trust = std::env::var("GIT_DIFFTOOL_TRUST_EXIT_CODE").as_deref() == Ok("true");

    // Tool resolution happens before the dir-diff branch in the script, so its
    // failure mode wins over everything below.
    if extcmd.is_none() {
        let named = env_nonempty("GIT_DIFF_TOOL");
        if named.is_none() {
            bail!(
                "unsupported invocation: without GIT_DIFFTOOL_EXTCMD the helper must run \
                 git-mergetool--lib's get_merge_tool, which reads diff.tool/merge.tool and then \
                 guesses from the tools installed under $(git --exec-path)/mergetools/ — no such \
                 tool database exists in the vendored crates \
                 (ported: the GIT_DIFFTOOL_EXTCMD path, and GIT_DIFF_TOOL with fewer than 7 args)"
            );
        }
        if dirdiff || args.len() > GROUP - 1 {
            bail!(
                "unsupported invocation: launching tool {:?} requires git-mergetool--lib's \
                 initialize_merge_tool/run_merge_tool, i.e. that tool's diff_cmd from \
                 $(git --exec-path)/mergetools/, which the vendored crates do not provide \
                 (ported: the GIT_DIFFTOOL_EXTCMD path)",
                named.unwrap_or_default()
            );
        }
        // GIT_DIFF_TOOL set, nothing to launch: the loop never runs.
        return Ok(ExitCode::SUCCESS);
    }
    let extcmd = extcmd.unwrap_or_default();

    if dirdiff {
        // `use_ext_cmd` is not consulted in the dir-diff branch, so `merge_tool`
        // is empty there and `setup_tool ""` falls through to `setup_user_tool`,
        // which reports the unset `difftool.<tool>.cmd` key.
        eprintln!("error: difftool..cmd not set for tool ''");
        return Ok(ExitCode::from(1));
    }

    let prompt = should_prompt();
    let counter = std::env::var("GIT_DIFF_PATH_COUNTER").unwrap_or_default();
    let total = std::env::var("GIT_DIFF_PATH_TOTAL").unwrap_or_default();

    let mut i = 0;
    while args.len() - i > GROUP - 1 {
        // MERGED/BASE = the work-tree path, LOCAL = a/, REMOTE = b/.
        let merged = &args[i];
        let local = &args[i + 1];
        let remote = &args[i + 4];

        let status = launch(&extcmd, merged, local, remote, prompt, &counter, &total)?;

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

/// The script's `launch_merge_tool` for the `use_ext_cmd` case, returning the
/// status the script would record in `$?`.
fn launch(
    extcmd: &str,
    merged: &str,
    local: &str,
    remote: &str,
    prompt: bool,
    counter: &str,
    total: &str,
) -> Result<i32> {
    if prompt {
        print!("\nViewing ({counter}/{total}): '{merged}'\nLaunch '{extcmd}' [Y/n]? ");
        std::io::stdout().flush()?;

        match read_reply()? {
            // `read ans || return` — at end of input the function returns the
            // failing read's status without launching anything.
            None => return Ok(1),
            // `test "$ans" = n` — a bare `return` after a true `test` is 0.
            Some(ans) if ans == "n" => return Ok(0),
            Some(_) => {}
        }
    }

    // `eval $GIT_DIFFTOOL_EXTCMD '"$LOCAL"' '"$REMOTE"'` with the surrounding
    // variables in scope. Running the same `eval` in a child `sh` keeps the
    // word-splitting of the unquoted command, the quoting of the two paths, and
    // the visibility of $LOCAL/$REMOTE/$MERGED/$BASE identical, and `BASE` is
    // the only one upstream exports.
    const SCRIPT: &str = r#"LOCAL="$1"
REMOTE="$2"
MERGED="$3"
BASE="$3"
export BASE
eval $4 '"$LOCAL"' '"$REMOTE"'"#;

    let status = Command::new("sh")
        .arg("-c")
        .arg(SCRIPT)
        .arg("sh")
        .arg(local)
        .arg(remote)
        .arg(merged)
        .arg(extcmd)
        .stdin(Stdio::inherit())
        .status()?;
    Ok(wait_status(status))
}

/// One line of the user's reply, with the trimming a POSIX `read ans` performs
/// (leading/trailing IFS whitespace). `None` marks the failing read at end of
/// input — including a final line with no terminating newline, which `read`
/// also reports as a failure.
fn read_reply() -> Result<Option<String>> {
    let mut buf = Vec::new();
    let mut stdin = std::io::stdin().lock();
    let mut byte = [0u8; 1];
    loop {
        match stdin.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => buf.push(byte[0]),
            Err(e) => return Err(e.into()),
        }
    }
    let s = String::from_utf8_lossy(&buf)
        .trim_matches(|c| c == ' ' || c == '\t')
        .to_owned();
    Ok(Some(s))
}

/// The script's `should_prompt`: `difftool.prompt`, falling back to
/// `mergetool.prompt`, falling back to true; then inverted through
/// `GIT_DIFFTOOL_NO_PROMPT` / `GIT_DIFFTOOL_PROMPT`.
fn should_prompt() -> bool {
    let prompt = config_bool("difftool.prompt")
        .or_else(|| config_bool("mergetool.prompt"))
        .unwrap_or(true);
    if prompt {
        env_nonempty("GIT_DIFFTOOL_NO_PROMPT").is_none()
    } else {
        env_nonempty("GIT_DIFFTOOL_PROMPT").is_some()
    }
}

/// A boolean config value, or `None` when unset. The helper also runs outside a
/// repository, so fall back to the system/global files.
///
/// Upstream reads these with `git config --bool`, whose failure on a malformed
/// value is swallowed by a `||` fallback; reading such a value as unset matches
/// that.
fn config_bool(key: &str) -> Option<bool> {
    if let Some(v) = gix::discover(".")
        .ok()
        .and_then(|repo| repo.config_snapshot().boolean(key))
    {
        return Some(v);
    }
    gix::config::File::from_globals()
        .ok()
        .and_then(|f| f.boolean(key).ok().flatten())
}

/// An environment variable, treating unset and empty alike — the script tests
/// these with `test -n`.
fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// The `$?` a shell would see for a finished child: its exit code, or `128 + n`
/// when it died of signal `n`.
fn wait_status(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(0)
    }
    #[cfg(not(unix))]
    {
        128
    }
}
