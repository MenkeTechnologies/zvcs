//! Running a program that is not us: the dashed-external dispatch (`git foo` →
//! exec `git-foo` from PATH) and the shell every `use_shell` child goes through.
//!
//! [`try_dashed`] is a faithful port of git.c's `execv_dashed_external`: once a
//! verb proves to be neither a builtin nor an alias, git looks for a `git-<verb>`
//! executable on PATH and execs it before giving up via `help_unknown_cmd`. This
//! is the mechanism every third-party subcommand relies on — `git fuzzy`,
//! `git lfs`, `git flow`, `git absorb`, `git town`, … — so shadowing stock `git`
//! without it silently breaks them all (git-fuzzy calls `git fuzzy helper` on
//! every keystroke and preview, recursing through whichever `git` is on PATH).
//!
//! We exec (replace this process) rather than spawn+wait, matching git: the
//! external owns the terminal outright — which a full-screen fzf TUI needs — and
//! its signals and exit status flow straight through with no intermediary.
//!
//! [`prepare_shell_cmd`] and [`shell`] are the other half: every hook,
//! `!`-alias, textconv/clean/smudge filter, pager, editor, mergetool, difftool,
//! `filter-branch` script, `submodule foreach` body and `rebase --exec` line runs
//! through the *one* shell named by [`SHELL_PATH`]. Resolving `sh` on PATH
//! instead would hand a user whose PATH front-loads a different `sh` a different
//! interpreter than git uses, for every one of those.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

/// Try to run `git-<cmd> <args>` from PATH. Returns:
///   * never (this process is replaced) when the external exists and execs;
///   * `Some(FAILURE)` when it exists but cannot be executed;
///   * `None` when no such external is on PATH — the caller then falls through to
///     autocorrect / "not a git command", exactly as git's `help_unknown_cmd`
///     does after `execv_dashed_external` fails with `ENOENT`.
pub fn try_dashed(cmd: &str, args: &[String]) -> Option<ExitCode> {
    let exe = format!("git-{cmd}");
    // `Command` PATH-searches a slash-free program name (execvp semantics), so a
    // bare `git-<cmd>` resolves against PATH just as git's own lookup does.
    let err = Command::new(&exe).args(args).exec();
    // `exec` returns only on failure. A missing external is the ordinary case
    // (the verb was simply a typo) — stay silent and let the caller diagnose it.
    if err.kind() == std::io::ErrorKind::NotFound {
        return None;
    }
    // It exists but is not runnable (not executable, bad interpreter, …) — git
    // reports this rather than pretending the command is unknown.
    eprintln!("zvcs: cannot exec '{exe}': {err}");
    Some(ExitCode::FAILURE)
}

// ---------------------------------------------------------------------------
// the shell (run-command.c)
// ---------------------------------------------------------------------------

/// git's `SHELL_PATH` — the shell every child git runs "through a shell" is run
/// through, and the value `git version --build-options` reports.
///
/// git fixes this at compile time (`SHELL_PATH = /bin/sh` in its Makefile) and
/// reaches it through `git_shell_path()`, which on every non-Windows platform is
/// nothing but `xstrdup(SHELL_PATH)`. It is deliberately absolute: git never
/// PATH-resolves the bare name `sh`, so the interpreter a hook or an `!`-alias
/// gets does not change when a user puts another `sh` earlier on PATH.
pub const SHELL_PATH: &str = "/bin/sh";

/// `prepare_shell_cmd()`'s metacharacter set, byte for byte:
/// `strcspn(argv[0], "|&;<>()$`\\\"' \t\n*?[#~=%")`. A command word containing
/// any of these needs a shell to mean what it says; one containing none of them
/// is just a program name and git execs it directly.
const SHELL_META: &[u8] = b"|&;<>()$`\\\"' \t\n*?[#~=%";

/// Whether `word` would send git's `prepare_shell_cmd()` down the shell branch.
pub fn needs_shell(word: &[u8]) -> bool {
    word.iter().any(|b| SHELL_META.contains(b))
}

/// A bare `Command` on [`SHELL_PATH`], for the places that are transcriptions of
/// a git *shell script* rather than a `use_shell` child — `git-mergetool.sh`,
/// `git-difftool--helper.sh`, `git-web--browse.sh`, `git-filter-branch.sh`. Those
/// scripts pick their own argv shape (a fixed script body, positional parameters
/// the script names, `sh -s` on stdin), so they cannot go through
/// [`prepare_shell_cmd`] — but the shell they run in is the same one, because
/// git substitutes `SHELL_PATH` into their `#!` line at build time.
pub fn shell() -> Command {
    Command::new(SHELL_PATH)
}

/// Faithful port of `prepare_shell_cmd()` (run-command.c), the transform git
/// applies to every child with `use_shell = 1`.
///
/// `cmd` is `argv[0]`, the command word; `args` is `argv[1..]`.
///
///   * No shell metacharacter in `cmd` — git leaves the argv alone and execs the
///     program directly. `run_command` then PATH-resolves a slash-free `argv[0]`
///     (`prepare_cmd`'s `locate_in_PATH`), which is exactly what `Command::new`
///     does for a slash-free program.
///   * A metacharacter is present — git prepends `<SHELL_PATH> -c <script>` and
///     then pushes the *whole* argv after it, so `$0` is the command word itself
///     and the caller's arguments start at `$1`. The script is `<cmd> "$@"`,
///     except with no arguments to substitute, where git skips "the `"$@"` magic"
///     and passes `cmd` alone.
///
/// Bytes are carried through as bytes: a command word that is not valid UTF-8
/// (a hook path, a `.gitattributes` filter) reaches the shell unchanged.
pub fn prepare_shell_cmd<I, S>(cmd: &OsStr, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut args = args.into_iter().peekable();
    if !needs_shell(cmd.as_bytes()) {
        let mut c = Command::new(cmd);
        c.args(args);
        return c;
    }
    let mut c = shell();
    c.arg("-c");
    if args.peek().is_none() {
        c.arg(cmd);
    } else {
        let mut script = cmd.to_os_string();
        script.push(" \"$@\"");
        c.arg(script);
    }
    // `strvec_pushv(out, argv)` — argv[0] lands on `$0`, argv[1..] on `"$@"`.
    c.arg(cmd);
    c.args(args);
    c
}

/// [`prepare_shell_cmd`] for a command word that is already a `&str`.
pub fn prepare_shell_cmd_str<I, S>(cmd: &str, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    prepare_shell_cmd(OsStr::new(cmd), args)
}

/// The empty `argv[1..]` for a one-word `use_shell` child, spelled so callers do
/// not have to annotate the element type of an empty array.
pub const NO_ARGS: [&OsStr; 0] = [];

#[cfg(test)]
mod tests {
    use super::*;

    /// The program and argv a built `Command` would exec, as strings.
    fn shape(c: &Command) -> (String, Vec<String>) {
        (
            c.get_program().to_string_lossy().into_owned(),
            c.get_args().map(|a| a.to_string_lossy().into_owned()).collect(),
        )
    }

    #[test]
    fn metacharacter_set_matches_git() {
        // Every byte of git's strcspn set, and a sample that contains none.
        for b in b"|&;<>()$`\\\"' \t\n*?[#~=%" {
            assert!(needs_shell(&[*b]), "byte {b:?} should force the shell");
        }
        for word in ["less", "vi", "cat-file", "/usr/local/bin/tool", "a+b", "x]y", "@{1}"] {
            assert!(!needs_shell(word.as_bytes()), "{word} should exec directly");
        }
    }

    #[test]
    fn bare_program_execs_directly() {
        // git: no metacharacter → `strvec_pushv(out, argv)` only, no shell.
        let (prog, args) = shape(&prepare_shell_cmd_str("less", ["a", "b"]));
        assert_eq!(prog, "less");
        assert_eq!(args, ["a", "b"]);
    }

    #[test]
    fn metacharacter_without_arguments_skips_the_dollar_at_magic() {
        // git: `if (!argv[1]) strvec_push(out, argv[0]);` then pushv(argv).
        let (prog, args) = shape(&prepare_shell_cmd_str("less -S", NO_ARGS));
        assert_eq!(prog, SHELL_PATH);
        assert_eq!(args, ["-c", "less -S", "less -S"]);
    }

    #[test]
    fn metacharacter_with_arguments_binds_dollar_at() {
        // git: `strvec_pushf(out, "%s \"$@\"", argv[0])` then pushv(argv), so the
        // command word is `$0` and the caller's first argument is `$1`.
        let (prog, args) = shape(&prepare_shell_cmd_str("vim -f", ["/tmp/COMMIT_EDITMSG"]));
        assert_eq!(prog, SHELL_PATH);
        assert_eq!(args, ["-c", r#"vim -f "$@""#, "vim -f", "/tmp/COMMIT_EDITMSG"]);
    }

    #[test]
    fn shell_is_absolute_not_path_resolved() {
        // The whole point: a `sh` earlier on PATH must be unreachable. Both the
        // scripted-transcription helper and the `use_shell` one name the same
        // absolute interpreter, and it has a directory separator, so neither
        // `Command`'s execvp nor git's `locate_in_PATH` ever consults PATH.
        assert_eq!(SHELL_PATH, "/bin/sh");
        assert!(SHELL_PATH.starts_with('/'), "SHELL_PATH must be absolute");
        assert_eq!(shell().get_program(), SHELL_PATH);
        assert_eq!(prepare_shell_cmd_str("echo hi", NO_ARGS).get_program(), SHELL_PATH);
    }

    #[test]
    fn non_utf8_command_word_survives() {
        use std::os::unix::ffi::OsStrExt;
        // A hook path or filter command need not be valid UTF-8; git passes the
        // bytes through untouched, so lossy conversion here would corrupt it.
        let raw = OsStr::from_bytes(b"/tmp/h\xffook run");
        let c = prepare_shell_cmd(raw, ["arg"]);
        let args: Vec<&OsStr> = c.get_args().collect();
        assert_eq!(args[1].as_bytes(), b"/tmp/h\xffook run \"$@\"");
        assert_eq!(args[2].as_bytes(), b"/tmp/h\xffook run");
    }
}
