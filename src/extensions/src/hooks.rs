//! Client-side hook execution — the veto/notify hooks git runs around commits,
//! merges, pushes and rebases (`pre-commit`, `commit-msg`, `pre-push`,
//! `pre-rebase`, `pre-merge-commit`, `post-commit`, `post-merge`, …).
//!
//! A hook is the executable file `<hooks-dir>/<event>` (`core.hooksPath` or
//! `<git-dir>/hooks`). It runs in the worktree with `GIT_DIR` set, its stdout
//! pointed at stderr (as git does), and — for hooks that receive one — a payload
//! on stdin. A non-zero exit aborts the operation that invoked it.
//!
//! The commit hooks additionally get `GIT_INDEX_FILE` naming the index that
//! commit is being built from (`commit.c:1994`), which is not always the
//! repository's own; [`run_with_env`] carries it, and reports back whether a hook
//! was there to run at all, because `git commit` re-reads that index afterwards
//! precisely when one was (`builtin/commit.c:1101-1109`).
//!
//! This is git's `find_hook()` (`hook.c:26-64`) plus the `git_path()` machinery
//! it leans on (`path.c:387-431`), and it is the single lookup every hook site in
//! zvcs goes through — `git hook` included — so the path git would name and the
//! path zvcs stats can never drift apart.

use anyhow::Result;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

/// Locate `<hooks-dir>/<event>`, returning it only when it exists and is
/// executable. A present-but-non-executable file draws git's `advice.ignoredHook`
/// hint (`hook.c:48-62`), once per hook name per process.
pub(crate) fn find(repo: &gix::Repository, event: &str) -> Result<Option<PathBuf>> {
    let (path, shown) = paths(repo, event)?;
    let Ok(meta) = std::fs::metadata(&path) else {
        return Ok(None);
    };
    if meta.is_dir() {
        return Ok(None);
    }
    if meta.permissions().mode() & 0o111 != 0 {
        return Ok(Some(path));
    }
    // `hook.c` bakes the disable sentence into the message and calls plain
    // `advise()`, so there is no `Disable this message with …` trailer — but the
    // call *is* behind `advice_enabled()`, which `GIT_ADVICE=0` also squelches,
    // and behind a `string_list` of names already advised about, so a hook looked
    // up repeatedly in one process (receive-pack runs `update` once per ref) is
    // still only reported once.
    if !crate::advice::Advice::IgnoredHook.enabled_in(repo) {
        return Ok(None);
    }
    if !advise_once(event) {
        return Ok(None);
    }
    crate::advice::print_hint(&format!(
        "The '{shown}' hook was ignored because it's not set as executable.\n\
         You can disable this warning with `git config set advice.ignoredHook false`."
    ));
    Ok(None)
}

/// `hook.c:50-53`'s `advise_given` string list: true the first time this process
/// is asked about `event`, false afterwards.
fn advise_once(event: &str) -> bool {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
        .lock()
        .map(|mut s| s.insert(event.to_owned()))
        .unwrap_or(true)
}

/// The hook file as two strings: the path to stat and run, and the path git would
/// print for it.
///
/// They differ because git has already `chdir`'d to the top of the work tree by
/// the time it builds either one (`setup.c`), while zvcs stays where it was
/// invoked. So a git directory git spells `.git` reaches zvcs from a
/// subdirectory as `../.git`, and a relative `core.hooksPath` — which
/// `adjust_git_path()` splices in verbatim (`path.c:400-401`) — is resolved by
/// git against the work tree root, not against the current directory.
fn paths(repo: &gix::Repository, event: &str) -> Result<(PathBuf, String)> {
    let (dir, shown_dir) = match repo.config_snapshot().trusted_path("core.hooksPath")? {
        Some(p) => {
            let p = p.to_path_buf();
            let on_disk = match repo.workdir() {
                Some(top) if p.is_relative() => top.join(&p),
                _ => p.clone(),
            };
            (on_disk, p)
        }
        None => {
            // `hooks` is a common path (`path.c:101`), so a linked worktree shares
            // the main repository's hooks directory.
            let git_dir = repo.common_dir();
            (git_dir.join("hooks"), git_dir_as_git_spells_it(repo).join("hooks"))
        }
    };
    let shown = shown_dir.join(event).display().to_string();
    // `strbuf_cleanup_path()` (`path.c:52-57`) drops a leading `./`.
    let shown = shown.strip_prefix("./").unwrap_or(&shown).to_owned();
    Ok((dir.join(event), shown))
}

/// `repo->gitdir` as git spells it in messages: relative to the top of the work
/// tree for an ordinary discovered repository (`.git`, however deep the command
/// was run), and absolute when it lies outside the work tree — a separate git
/// directory, a linked worktree's common directory, or an absolute `GIT_DIR`.
fn git_dir_as_git_spells_it(repo: &gix::Repository) -> PathBuf {
    let git_dir = lexical_normalize(repo.common_dir());
    if git_dir.is_absolute() {
        return git_dir;
    }
    let (Some(top), Ok(cwd)) = (repo.workdir(), std::env::current_dir()) else {
        return git_dir;
    };
    let abs_git_dir = lexical_normalize(&cwd.join(&git_dir));
    let abs_top = lexical_normalize(&cwd.join(top));
    match abs_git_dir.strip_prefix(&abs_top) {
        Ok(rel) => rel.to_path_buf(),
        Err(_) => abs_git_dir,
    }
}

/// `path` as an absolute path, resolved against the current directory and then
/// collapsed textually.
///
/// Anything a hook is handed — the executable, its working directory, `GIT_DIR`,
/// `GIT_INDEX_FILE` — has to survive the child's `chdir` into the work tree root,
/// and gitoxide hands back exactly the relative paths the repository was
/// discovered with.
pub(crate) fn absolutize(path: &Path) -> PathBuf {
    let joined = match std::env::current_dir() {
        Ok(cwd) if path.is_relative() => cwd.join(path),
        _ => path.to_owned(),
    };
    lexical_normalize(&joined)
}

/// Collapse `.` and `..` textually, without touching the filesystem. gitoxide
/// hands back a linked worktree's common directory as
/// `…/.git/worktrees/<id>/../..`; git prints the collapsed form.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // `/..` is `/`; a leading `..` has nothing to cancel against.
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// What a hook run amounted to: whether a hook was actually there to run, and
/// whether it let the operation proceed.
///
/// The two are distinct because git keeps them distinct. `run_commit_hook()`
/// takes an `int *invoked_hook` out-parameter (`commit.h:379-380`) that
/// `run_hooks_opt()` clears to 0 before the lookup (`hook.c:823-824`) and sets to
/// 1 only once a hook is about to be executed (`hook.c:659-660`) — and
/// `prepare_to_commit()` keys its post-hook index re-read on that flag, not on
/// the exit status (`builtin/commit.c:1101`). A repository with no `pre-commit`
/// hook therefore keeps the index it already read, which is the difference
/// between "nothing ran" and "something ran and changed nothing".
pub struct Outcome {
    /// A hook file was found, was executable, and was executed —
    /// `*opt->invoked_hook = 1` (`hook.c:659-660`).
    pub invoked: bool,
    /// The operation may proceed: no hook, or a hook that exited 0.
    pub ok: bool,
}

/// Run the client-side hook `event` if present, feeding `args` and (optionally)
/// `stdin`. Returns `Ok(true)` to proceed — no hook installed, or the hook exited
/// 0 — and `Ok(false)` when the hook exited non-zero, which the caller treats as
/// a veto and aborts the operation, exactly as git does.
pub fn run(
    repo: &gix::Repository,
    event: &str,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<bool> {
    Ok(run_with_env(repo, event, args, stdin, &[])?.ok)
}

/// [`run`], plus the environment git exports to the hook and the `invoked_hook`
/// flag git needs back from it.
///
/// `env` is git's `strvec_pushf(&opt.env, "GIT_INDEX_FILE=%s", index_file)`
/// (`commit.c:1994`): the commit hooks do not see whatever index the caller
/// happens to discover, they see *the index this commit is being built from*,
/// which for `--only` is a temporary file and for `-a`/`-i` is the locked one.
/// A hook that runs `git add` therefore stages into the same index the commit
/// will read back, which is the whole reason an auto-formatter hook works.
pub fn run_with_env(
    repo: &gix::Repository,
    event: &str,
    args: &[&str],
    stdin: Option<&[u8]>,
    env: &[(&str, &Path)],
) -> Result<Outcome> {
    let Some(path) = find(repo, event)? else {
        return Ok(Outcome { invoked: false, ok: true });
    };

    // Hooks run in the worktree (or the git dir for a bare repo) with GIT_DIR set,
    // and git points their stdout at stderr so hook chatter never pollutes the
    // command's own stdout.
    //
    // Every path handed to the child is absolute first. git can afford relative
    // ones because `setup.c` has already `chdir`'d it to the top of the work tree,
    // so its `.git/hooks/pre-commit` means the same thing before and after the
    // spawn; zvcs stays where it was invoked, and a repository discovered from a
    // subdirectory spells the same file `../.git/hooks/pre-commit`. Handing *that*
    // to `Command` while also moving the child's cwd to the work tree root points
    // the exec one directory above the repository, and `git commit` from any
    // subdirectory of a repository with any hook installed failed outright:
    //
    // ```text
    // $ cd deep && git commit -m m
    // zvcs: commit: No such file or directory (os error 2)
    // ```
    let workdir = absolutize(repo.workdir().unwrap_or_else(|| repo.git_dir()));
    let program = absolutize(&path);
    let git_dir = absolutize(repo.git_dir());
    let mut cmd = Command::new(&program);
    cmd.args(args)
        .current_dir(&workdir)
        .env("GIT_DIR", &git_dir)
        .envs(env.iter().map(|(k, v)| (*k, v.as_os_str())))
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

    // `start_command()`'s `fflush(NULL)` (run-command.c:743) — a hook's output
    // must not overtake the buffered output of the command that ran it.
    crate::cstdio::before_spawn();
    let mut child = cmd.spawn()?;
    if let Some(data) = stdin {
        if let Some(mut sink) = child.stdin.take() {
            sink.write_all(data)?;
        }
    }
    Ok(Outcome { invoked: true, ok: child.wait()?.success() })
}
