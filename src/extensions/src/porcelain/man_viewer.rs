//! `man.viewer`, `man.<tool>.path` and `man.<tool>.cmd` — the viewer chain
//! `git help -m` walks before it falls back to `man`.
//!
//! Port of the man-viewer half of `builtin/help.c`: `add_man_viewer()`
//! (help.c:350-357), `add_man_viewer_info()` (help.c:405-425),
//! `exec_viewer()` (help.c:502-516) and `show_man_page()` (help.c:518-532).
//!
//! ### The chain
//!
//! `show_man_page()` is a fall-through, not a choice. Every `man.viewer` in the
//! configuration is tried in the order the configuration lists it, then
//! `$GIT_MAN_VIEWER`, then the literal `man`, and only if all of them decline
//! does git `die(_("no man viewer handled the request"))`. "Declining" is
//! precise in the C: each `exec_*` helper ends in `execlp()`, so a viewer that
//! starts *replaces the process* and its exit status is git's, while a viewer
//! that cannot start makes `execlp` return and the loop continues to the next
//! candidate. This port spawns instead of `exec`ing — it has an atexit-time
//! Trace2 record to write — but keeps the distinction: a spawn that fails is a
//! `warning: failed to exec '<path>'` and the next viewer is tried, a spawn that
//! succeeds ends the walk with the child's status.
//!
//! ### Three viewers are special, the rest are commands
//!
//! `supported_man_viewer()` (help.c:359-364) names `man`, `woman` and
//! `konqueror`. For those three git knows how to build a command line, so
//! `man.<tool>.path` overrides *where the program is* and `man.<tool>.cmd` is
//! refused with a warning. For every other name git knows nothing, so
//! `man.<tool>.cmd` supplies the whole shell command and `man.<tool>.path` is
//! the one refused. The two warnings (help.c:384-386, help.c:396-398) are the
//! only diagnostic a user gets for putting the value under the wrong key, so
//! they are reproduced verbatim, including their embedded newline.
//!
//! Name matching is case-insensitive throughout — `strcasecmp` in
//! `get_man_viewer_info()`, `strncasecmp` in `supported_man_viewer()` — so
//! `[man "MAN"] path = …` configures the `man` viewer.

use crate::external;
use anyhow::Result;
use std::path::Path;
use std::process::{Command, ExitCode};

/// `supported_man_viewer()` (help.c:359-364): the viewers git can drive itself.
/// Their command lines are built in code, so they take a `path` and reject a
/// `cmd`; every other name is the other way round.
const SUPPORTED: &[&str] = &["man", "woman", "konqueror"];

/// The configuration `show_man_page()` walks: the ordered `man.viewer` list and
/// the per-tool `path`/`cmd` overrides, which git keeps in two separate lists
/// filled by the same config callback (`git_help_config()`, help.c:427-453).
#[derive(Default)]
pub struct Viewers {
    /// `man.viewer`, in configuration order — `add_man_viewer()` appends to the
    /// tail of the list, so an earlier file's value is tried first.
    order: Vec<String>,
    /// `man.<tool>.path` / `man.<tool>.cmd`, as `(tool, command-or-path)`.
    /// `do_add_man_viewer_info()` pushes onto the *head*, so a later definition
    /// of the same tool wins the `strcasecmp` lookup.
    info: Vec<(String, String)>,
}

impl Viewers {
    /// `get_man_viewer_info()` (help.c:244-254): the `path`/`cmd` recorded for
    /// `name`, matched case-insensitively.
    fn info(&self, name: &str) -> Option<&str> {
        self.info
            .iter()
            .find(|(tool, _)| tool.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Read the `[man]` section the way `git_help_config()` does: `man.viewer`
/// appends to the viewer order, `man.<tool>.path` and `man.<tool>.cmd` record a
/// per-tool override, and anything else under `man.` is ignored.
///
/// git sees one flat stream of key/value pairs in file order and this reads two
/// ordered views of the same section, which is the same thing for the two lists
/// that matter: `viewer` values keep file order, and per-tool overrides are
/// newest-first so the last definition wins.
pub fn load(file: &gix::config::File) -> Viewers {
    let mut out = Viewers::default();
    let Some(sections) = file.sections_by_name("man") else {
        return out;
    };
    for section in sections {
        match section.header().subsection_name() {
            // `[man] viewer = <tool>` — the chain itself.
            None => {
                for value in section.values("viewer") {
                    let value = String::from_utf8_lossy(&value).into_owned();
                    if !value.is_empty() {
                        out.order.push(value);
                    }
                }
            }
            // `[man "<tool>"] path|cmd = <value>`.
            Some(tool) => {
                let tool = tool.to_string();
                let supported = SUPPORTED.iter().any(|s| s.eq_ignore_ascii_case(&tool));
                for (key, supported_wanted, hint) in
                    [("path", true, "cmd"), ("cmd", false, "path")]
                {
                    let Some(value) = section.value(key) else {
                        continue;
                    };
                    let value = String::from_utf8_lossy(&value).into_owned();
                    if supported != supported_wanted {
                        // `add_man_viewer_path()` / `add_man_viewer_cmd()`: the
                        // value is dropped, not applied, and the warning names
                        // the key that would have worked.
                        //
                        // The name it quotes is `<tool>.<subkey>`, not the tool.
                        // `parse_config_key()` hands back `name` as a pointer
                        // *into* the variable with a separate `namelen`, and the
                        // warning prints it with a plain `%s` — so everything
                        // from the subsection to the end of the key comes out:
                        //
                        // ```text
                        // $ git -c man.foo.path=/bin/echo help -m status
                        // warning: 'foo.path': path for unsupported man viewer.
                        // ```
                        //
                        // Reproduced rather than corrected: a user grepping for
                        // that line, or a test pinning it, matches stock.
                        let what =
                            if supported_wanted { "path for unsupported" } else { "cmd for supported" };
                        eprintln!(
                            "warning: '{tool}.{key}': {what} man viewer.\nPlease consider using 'man.<tool>.{hint}' instead."
                        );
                        continue;
                    }
                    out.info.insert(0, (tool.clone(), value));
                }
            }
        }
    }
    out
}

/// `setup_man_path()` (help.c:481-500): put this installation's man directory in
/// front of `$MANPATH`, keeping the trailing `:` that lets `man` fall through to
/// the system-wide path. `extra` is the generated-page root for a superset verb,
/// which has no system-wide copy to fall through to.
fn setup_man_path(extra: Option<&Path>) {
    let mut new_path = String::new();
    for dir in extra.into_iter().chain(std::iter::once(
        crate::superset::manpage::man_dir().as_path(),
    )) {
        new_path.push_str(&dir.display().to_string());
        new_path.push(':');
    }
    if let Some(old) = std::env::var_os("MANPATH") {
        new_path.push_str(&old.to_string_lossy());
    }
    std::env::set_var("MANPATH", new_path);
}

/// One viewer attempt. `Some(code)` means the viewer ran and this is the
/// process' exit status — git would have been replaced by it. `None` means it
/// declined (git's `execlp` returned, or the viewer is unknown) and the caller
/// must try the next candidate.
fn exec_viewer(viewers: &Viewers, name: &str, page: &str) -> Option<u8> {
    let info = viewers.info(name);
    if name.eq_ignore_ascii_case("man") {
        return exec_man_man(info, page);
    }
    if name.eq_ignore_ascii_case("woman") {
        return exec_woman_emacs(info, page);
    }
    if name.eq_ignore_ascii_case("konqueror") {
        return exec_man_konqueror(info, page);
    }
    match info {
        Some(cmd) => exec_man_cmd(cmd, page),
        None => {
            // `warning(_("'%s': unknown man viewer."), name)` — a viewer named
            // by `man.viewer` with no `man.<tool>.cmd` to run.
            eprintln!("warning: '{name}': unknown man viewer.");
            None
        }
    }
}

/// `exec_man_man()` (help.c:333-339): `execlp(path ? path : "man", "man", page)`.
/// The `path` override changes the program without changing `argv[0]`.
fn exec_man_man(path: Option<&str>, page: &str) -> Option<u8> {
    let program = path.unwrap_or("man");
    run(Command::new(program).arg(page), program)
}

/// `exec_man_cmd()` (help.c:341-348): the configured command with the page name
/// appended, run through `$SHELL_PATH -c`. The value is a command *line*, not a
/// program, so `man.foo.cmd = "printf '%s\n'"` runs printf with the page as its
/// argument.
fn exec_man_cmd(cmd: &str, page: &str) -> Option<u8> {
    let shell_cmd = format!("{cmd} {page}");
    let mut command = external::shell();
    command.arg("-c").arg(&shell_cmd);
    match command.status() {
        Ok(status) => Some(crate::porcelain::help::exit_status_code(status)),
        Err(_) => {
            // `warning(_("failed to exec '%s'"), cmd)` — no errno in this one.
            eprintln!("warning: failed to exec '{cmd}'");
            None
        }
    }
}

/// `exec_woman_emacs()` (help.c:296-309): emacs' WoMan, driven through
/// `emacsclient -e '(woman "<page>")'`, but only after
/// `check_emacsclient_version()` proves the client is at least version 22 —
/// older ones cannot evaluate `-e` forms.
fn exec_woman_emacs(path: Option<&str>, page: &str) -> Option<u8> {
    check_emacsclient_version(path)?;
    let program = path.unwrap_or("emacsclient");
    run(Command::new(program).arg("-e").arg(format!("(woman \"{page}\")")), program)
}

/// `check_emacsclient_version()` (help.c:256-294): `emacsclient --version`
/// prints to *stderr* and exits non-zero, so the output is what is inspected,
/// not the status. Anything that does not start with `emacsclient`, or a major
/// version below 22, is an `error:` and the viewer is skipped.
fn check_emacsclient_version(path: Option<&str>) -> Option<()> {
    let out = Command::new(path.unwrap_or("emacsclient")).arg("--version").output();
    let Ok(out) = out else {
        eprintln!("error: Failed to start emacsclient.");
        return None;
    };
    // git reads 20 bytes of the child's stderr; `stdout_to_stderr` folds the
    // two streams together first, so either one can carry the banner.
    let mut text = String::from_utf8_lossy(&out.stderr).into_owned();
    if text.is_empty() {
        text = String::from_utf8_lossy(&out.stdout).into_owned();
    }
    let Some(rest) = text.strip_prefix("emacsclient") else {
        eprintln!("error: Failed to parse emacsclient version.");
        return None;
    };
    // `atoi()` on the remainder: leading spaces skipped, digits taken, garbage
    // reads as 0.
    let digits: String =
        rest.trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
    let version: i32 = digits.parse().unwrap_or(0);
    if version < 22 {
        eprintln!("error: emacsclient version '{version}' too old (< 22).");
        return None;
    }
    Some(())
}

/// `exec_man_konqueror()` (help.c:311-331): konqueror is driven through
/// `kfmclient newTab man:<page>(1)`, and only inside a graphical session — with
/// `$DISPLAY` empty the viewer declines without running anything. A
/// `man.konqueror.path` ending in `/konqueror` is rewritten to the `kfmclient`
/// beside it, and `argv[0]` becomes that program's basename.
fn exec_man_konqueror(path: Option<&str>, page: &str) -> Option<u8> {
    let display = std::env::var("DISPLAY").unwrap_or_default();
    if display.is_empty() {
        return None;
    }
    let program = match path {
        Some(p) => match p.strip_suffix("/konqueror") {
            Some(dir) => format!("{dir}/kfmclient"),
            None => p.to_string(),
        },
        None => "kfmclient".to_string(),
    };
    run(Command::new(&program).arg("newTab").arg(format!("man:{page}(1)")), &program)
}

/// Spawn a viewer git would have `execlp`'d, mapping the two outcomes onto
/// git's: it ran (this process is done), or it could not start
/// (`warning_errno(_("failed to exec '%s'"), path)` and try the next).
fn run(command: &mut Command, program: &str) -> Option<u8> {
    match command.status() {
        Ok(status) => Some(crate::porcelain::help::exit_status_code(status)),
        Err(e) => {
            eprintln!("warning: failed to exec '{program}': {}", errno_text(&e));
            None
        }
    }
}

/// `strerror(errno)` without the ` (os error <n>)` tail Rust appends, so a
/// `warning_errno` reads as git's does.
fn errno_text(e: &std::io::Error) -> String {
    let text = e.to_string();
    text.split(" (os error ").next().unwrap_or(&text).to_string()
}

/// `show_man_page()` (help.c:518-532): every configured viewer in turn, then
/// `$GIT_MAN_VIEWER`, then `man`, then die.
///
/// `extra_man_root` is this port's addition to `setup_man_path()`: the directory
/// a superset (`z*`) verb's page was just generated into. Putting it on
/// `$MANPATH` rather than passing `man -M` is what lets the whole chain work for
/// those pages too — `man.<tool>.cmd` viewers take a page *name*, and only the
/// environment can tell them where to find it.
pub fn show_man_page(
    file: &gix::config::File,
    page: &str,
    extra_man_root: Option<&Path>,
) -> Result<ExitCode> {
    let viewers = load(file);
    setup_man_path(extra_man_root);

    let fallback = std::env::var("GIT_MAN_VIEWER").ok().filter(|v| !v.is_empty());
    let chain = viewers
        .order
        .iter()
        .map(String::as_str)
        .chain(fallback.as_deref())
        .chain(std::iter::once("man"))
        .collect::<Vec<_>>();

    for name in chain {
        // The viewer inherits this process' stdout, so anything already
        // buffered has to be on the wire before it starts.
        use std::io::Write;
        std::io::stdout().flush().ok();
        if let Some(code) = exec_viewer(&viewers, name, page) {
            return Ok(ExitCode::from(code));
        }
    }
    // `die(_("no man viewer handled the request"))` — reported here rather than
    // returned as an error so it carries git's `fatal: ` prefix and exit 128
    // instead of this port's `zvcs: help: …`/exit 1 wrapper.
    eprintln!("fatal: no man viewer handled the request");
    Ok(ExitCode::from(crate::fatal::EXIT_FATAL))
}
