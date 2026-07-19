use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

use crate::lock::RepoLock;

/// `git init` — create an empty repository (worktree or `--bare`).
///
/// Ported onto gitoxide's `gix::init` / `gix::init_bare`, which lay down the
/// same on-disk layout git does (`.git/{HEAD,config,objects,refs,hooks,info}`)
/// with an unborn `HEAD` pointing at the default branch (`init.defaultBranch`
/// config, else `main`). Output mirrors stock git:
///   * fresh repo:    `Initialized empty Git repository in <gitdir>/`
///   * existing repo: `Reinitialized existing Git repository in <gitdir>/`
///
/// Supported invocation forms:
///   * `git init [<directory>]`
///   * `git init --bare [<directory>]`
///   * `git init -b <name>` / `--initial-branch=<name>`  (sets `HEAD` symref)
///   * `git init -q` / `--quiet`                          (suppresses the line)
///   * `--` to terminate option parsing
///
/// # Deviations (surfaced honestly, never faked)
///   * Reinitialization prints the git message and succeeds but does NOT
///     re-copy missing template hooks/`info/exclude` (gix exposes no reinit
///     path); a repo whose samples were deleted is not repopulated. The
///     overwhelmingly common `git init` in an already-initialized repo is
///     unaffected because those files already exist.
///   * gix refuses `--bare` into a non-empty directory (`DirectoryNotEmpty`),
///     where stock git permits it. The common `git init --bare <newdir>` works;
///     the non-empty case surfaces the gix error rather than a fake success.
pub fn init(args: &[String]) -> Result<ExitCode> {
    let mut bare = false;
    let mut quiet = false;
    let mut initial_branch: Option<String> = None;
    let mut directory: Option<String> = None;
    let mut positional_only = false;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if positional_only || !arg.starts_with('-') || arg == "-" {
            if directory.is_some() {
                anyhow::bail!("too many arguments, expected at most one directory");
            }
            directory = Some(arg.clone());
            i += 1;
            continue;
        }
        match arg.as_str() {
            "--" => positional_only = true,
            "--bare" => bare = true,
            "-q" | "--quiet" => quiet = true,
            "-b" | "--initial-branch" => {
                i += 1;
                let name = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("option `{arg}' requires a value"))?;
                initial_branch = Some(name.clone());
            }
            _ if arg.starts_with("--initial-branch=") => {
                initial_branch = Some(arg["--initial-branch=".len()..].to_string());
            }
            _ if arg.starts_with("-b") => {
                initial_branch = Some(arg[2..].to_string());
            }
            _ => anyhow::bail!("unknown option `{arg}'"),
        }
        i += 1;
    }

    let target = PathBuf::from(directory.as_deref().unwrap_or("."));

    // Detect an already-initialized repository at the target so we can emit the
    // `Reinitialized existing ...` line instead of failing. For a worktree repo
    // the git dir is `<target>/.git`; for a bare repo it is `<target>` itself,
    // recognized by its `HEAD` file at the root.
    let existing_git_dir: Option<PathBuf> = {
        let dot_git = target.join(".git");
        if dot_git.exists() {
            Some(dot_git)
        } else if target.join("HEAD").is_file() && target.join("objects").is_dir() {
            Some(target.clone())
        } else {
            None
        }
    };

    if let Some(git_dir) = existing_git_dir {
        if !quiet {
            println!(
                "Reinitialized existing Git repository in {}",
                display_git_dir(&git_dir)
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    // Create the repository. gix lays down the full template + config and returns
    // an opened handle with an unborn HEAD on the default branch.
    let repo = if bare {
        gix::init_bare(&target).map_err(|e| anyhow::anyhow!("{e}"))?
    } else {
        gix::init(&target).map_err(|e| anyhow::anyhow!("{e}"))?
    };

    // `-b <name>` / `--initial-branch=<name>`: repoint the unborn HEAD symref.
    // This is a ref mutation, so serialize it through the repo coordinator like
    // every other write command.
    if let Some(name) = initial_branch {
        let branch: FullName = format!("refs/heads/{name}")
            .try_into()
            .map_err(|e| anyhow::anyhow!("invalid initial branch name {name:?}: {e}"))?;
        let _lock = RepoLock::acquire(repo.git_dir());
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "init: set initial branch".into(),
                },
                expected: PreviousValue::Any,
                new: Target::Symbolic(branch),
            },
            name: "HEAD"
                .try_into()
                .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
            deref: false,
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    }

    if !quiet {
        println!(
            "Initialized empty Git repository in {}",
            display_git_dir(repo.git_dir())
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Render a git-dir path the way stock git does in the init message: an absolute,
/// symlink-resolved path with a trailing slash. Falls back to the given path when
/// canonicalization is unavailable (should not happen for a just-created dir).
fn display_git_dir(git_dir: &Path) -> String {
    let abs = std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_path_buf());
    format!("{}/", abs.display())
}
