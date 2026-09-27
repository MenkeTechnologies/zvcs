//! Port of `editor.c`: choosing the editor and running it.
//!
//! Every verb that hands the user a file to edit — `commit`, `tag`, `merge`,
//! `notes`, `branch --edit-description`, `add -e`, `add -p`'s hunk edit,
//! `replace --edit`, `config --edit`, `am -i`, `history`, `bugreport`, and the
//! rebase todo list through [`launch_sequence_editor`] — goes through this one
//! launcher, as every caller in git goes through `launch_specified_editor()`.
//!
//! The child starts where git's setup left git standing, with the `GIT_DIR`,
//! `GIT_WORK_TREE` and `GIT_PREFIX` setup exported
//! ([`crate::setup::export_to_child`]): git runs it with `p.dir` unset, and zvcs
//! never `chdir`s its own process.

use std::ffi::OsStr;
use std::path::Path;

/// `DEFAULT_EDITOR` (editor.c:17-19).
const DEFAULT_EDITOR: &str = "vi";

/// `is_terminal_dumb()` (editor.c:21-25): an unset `TERM` counts as dumb.
pub fn is_terminal_dumb() -> bool {
    std::env::var_os("TERM").is_none_or(|t| t == "dumb")
}

/// `git_editor()` (editor.c:27-46):
///
/// ```c
/// const char *editor = getenv("GIT_EDITOR");
/// int terminal_is_dumb = is_terminal_dumb();
///
/// if (!editor && editor_program)      editor = editor_program;
/// if (!editor && !terminal_is_dumb)   editor = getenv("VISUAL");
/// if (!editor)                        editor = getenv("EDITOR");
/// if (!editor && terminal_is_dumb)    return NULL;
/// if (!editor)                        editor = DEFAULT_EDITOR;
/// ```
///
/// `getenv` answers non-NULL for an *empty* variable, so `GIT_EDITOR=` selects
/// the empty editor rather than falling through to `core.editor`. `$VISUAL` is
/// skipped on a dumb terminal but `$EDITOR` is not, and a dumb terminal with
/// none of them set is the only way to get `None` (git's NULL).
///
/// `editor_program` is `core.editor` read by `git_default_core_config()` with
/// `git_config_string()` (environment.c), so it is taken verbatim. Without a
/// repository no configuration is consulted here.
pub fn git_editor(repo: Option<&gix::Repository>) -> Option<String> {
    select_editor(repo.and_then(|r| config_string(r, "core.editor")))
}

/// [`git_editor`] with `editor_program` supplied by a caller that reads its
/// configuration without a repository (`git var`).
pub fn select_editor(editor_program: Option<String>) -> Option<String> {
    let dumb = is_terminal_dumb();
    let mut editor = std::env::var("GIT_EDITOR").ok();
    if editor.is_none() {
        editor = editor_program;
    }
    if editor.is_none() && !dumb {
        editor = std::env::var("VISUAL").ok();
    }
    if editor.is_none() {
        editor = std::env::var("EDITOR").ok();
    }
    if editor.is_none() && dumb {
        return None;
    }
    Some(editor.unwrap_or_else(|| DEFAULT_EDITOR.to_string()))
}

/// `git_sequence_editor()` (editor.c:48-58): `$GIT_SEQUENCE_EDITOR`, then
/// `sequence.editor`, then [`git_editor`].
pub fn git_sequence_editor(repo: Option<&gix::Repository>) -> Option<String> {
    select_sequence_editor(
        repo.and_then(|r| config_string(r, "sequence.editor")),
        repo.and_then(|r| config_string(r, "core.editor")),
    )
}

/// [`git_sequence_editor`] with both configuration values supplied by the caller.
pub fn select_sequence_editor(sequence_editor: Option<String>, editor_program: Option<String>) -> Option<String> {
    std::env::var("GIT_SEQUENCE_EDITOR")
        .ok()
        .or(sequence_editor)
        .or_else(|| select_editor(editor_program))
}

fn config_string(repo: &gix::Repository, key: &str) -> Option<String> {
    repo.config_snapshot().string(key).map(|v| v.to_string())
}

/// git's `-1` from `launch_specified_editor()`: the `error:` line is already on
/// stderr, and what the command does next is the caller's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorFailed;

impl std::fmt::Display for EditorFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("editor failed")
    }
}

impl std::error::Error for EditorFailed {}

/// Extra environment for the editor, as git's `const char *const *env`:
/// `(name, Some(value))` sets, `(name, None)` unsets.
pub type EditorEnv<'a> = &'a [(&'a str, Option<&'a OsStr>)];

/// `launch_editor(path, NULL, env)` (editor.c:141-144).
pub fn launch_editor(repo: Option<&gix::Repository>, path: &Path, env: EditorEnv<'_>) -> Result<(), EditorFailed> {
    launch_specified_editor(repo, git_editor(repo), path, false, env).map(drop)
}

/// `launch_editor(path, &buffer, env)`: the file as the editor left it. Read
/// back even for the `:` editor, which leaves it as it was written.
pub fn launch_editor_read(
    repo: Option<&gix::Repository>,
    path: &Path,
    env: EditorEnv<'_>,
) -> Result<Vec<u8>, EditorFailed> {
    launch_specified_editor(repo, git_editor(repo), path, true, env).map(Option::unwrap_or_default)
}

/// `launch_sequence_editor(path, &buffer, env)` (editor.c:146-150).
pub fn launch_sequence_editor(
    repo: Option<&gix::Repository>,
    path: &Path,
    env: EditorEnv<'_>,
) -> Result<Vec<u8>, EditorFailed> {
    launch_specified_editor(repo, git_sequence_editor(repo), path, true, env).map(Option::unwrap_or_default)
}

/// `strbuf_edit_interactively()` (editor.c:152-181): write `buffer` to `name`
/// under the git directory, edit it, hand back the result and unlink the file.
///
/// Every failure is an `error()` naming the file as git spells it — relative to
/// where setup left git standing. A failed edit adds `could not edit '<path>'`
/// with whatever `errno` the editor run left, which after a child that merely
/// exited non-zero is 0.
pub fn edit_interactively(repo: &gix::Repository, buffer: &[u8], name: &str) -> Result<Vec<u8>, EditorFailed> {
    let path = repo.git_dir().join(name);
    let shown = crate::setup::git_path_spelled(repo, name);
    let shown = shown.display();
    let mut file = match std::fs::File::create(&path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("error: could not open '{shown}' for writing: {}", crate::external::strerror(&e));
            return Err(EditorFailed);
        }
    };
    if let Err(e) = std::io::Write::write_all(&mut file, buffer) {
        eprintln!("error: could not write to '{shown}': {}", crate::external::strerror(&e));
        return Err(EditorFailed);
    }
    drop(file);
    let edited = launch_editor_read(Some(repo), &path, &[]);
    if edited.is_err() {
        // Nothing in a child that ran and exited non-zero sets `errno`.
        let errno = std::io::Error::from_raw_os_error(0);
        eprintln!("error: could not edit '{shown}': {}", crate::external::strerror(&errno));
    }
    let _ = std::fs::remove_file(&path);
    edited
}

/// `launch_specified_editor()` (editor.c:60-139).
fn launch_specified_editor(
    repo: Option<&gix::Repository>,
    editor: Option<String>,
    path: &Path,
    read_back: bool,
    env: EditorEnv<'_>,
) -> Result<Option<Vec<u8>>, EditorFailed> {
    let Some(editor) = editor else {
        eprintln!("error: Terminal is dumb, but EDITOR unset");
        return Err(EditorFailed);
    };

    // `if (strcmp(editor, ":"))`: the no-op editor is recognised before any
    // child is built — nothing is spawned, not even the waiting hint.
    if editor != ":" {
        let dumb = is_terminal_dumb();
        let waiting = crate::advice::Advice::WaitingForEditor.enabled()
            && std::io::IsTerminal::is_terminal(&std::io::stderr());
        if waiting {
            // A dumb terminal cannot erase the line later, so it gets a newline.
            eprint!("hint: Waiting for your editor to close the file...{}", if dumb { '\n' } else { ' ' });
        }

        // `strbuf_realpath(&realpath, path, 1)` and `p.use_shell = 1`; the
        // child runs where setup left git, with what setup exported.
        let realpath = crate::setup::realpath(path);
        let mut cmd = crate::external::prepare_shell_cmd_str(&editor, [&realpath]);
        match repo {
            Some(repo) => crate::setup::export_to_child(repo, crate::setup::after_setup(repo).as_ref(), &mut cmd),
            // `setenv(GIT_PREFIX_ENVIRONMENT, "", 1)` outside a repository
            // (setup.c:2073-2076).
            None => {
                cmd.env("GIT_PREFIX", "");
            }
        }
        for (name, value) in env {
            match value {
                Some(v) => cmd.env(name, v),
                None => cmd.env_remove(name),
            };
        }

        // `start_command()`'s `fflush(NULL)` (run-command.c): the editor takes
        // the terminal, so nothing may still sit in our buffers.
        crate::cstdio::before_spawn();
        let status = match cmd.status() {
            Ok(status) => status,
            Err(e) => {
                // `start_command()` reports its own `cannot run` line first.
                eprintln!("error: cannot run {editor}: {}", crate::external::strerror(&e));
                eprintln!("error: unable to start editor '{editor}'");
                return Err(EditorFailed);
            }
        };
        if waiting && !dumb {
            // `term_clear_line()`.
            eprint!("\r\x1b[K");
        }
        if !status.success() {
            eprintln!("error: there was a problem with the editor '{editor}'");
            return Err(EditorFailed);
        }
    }

    if !read_back {
        return Ok(None);
    }
    match std::fs::read(path) {
        Ok(buf) => Ok(Some(buf)),
        Err(e) => {
            eprintln!(
                "error: could not read file '{}': {}",
                path.display(),
                crate::external::strerror(&e)
            );
            Err(EditorFailed)
        }
    }
}
