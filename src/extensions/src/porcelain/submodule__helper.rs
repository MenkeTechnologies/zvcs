//! `git submodule--helper` — the internal dispatcher behind `git submodule`.
//!
//! In git 2.55.0 this is a builtin whose only job is `parse_options` with
//! `PARSE_OPT_SUBCOMMAND`: it owns no options of its own, and every subcommand
//! it names is the same C function `git submodule` reaches. Fourteen
//! subcommands are registered (verified by probing `<cmd> -h` against git
//! 2.55.0 on Darwin): `clone`, `add`, `update`, `foreach`, `init`, `status`,
//! `sync`, `deinit`, `summary`, `push-check`, `absorbgitdirs`, `set-url`,
//! `set-branch`, `create-branch`, plus `gitdir`, `get-default-remote` and
//! `migrate-gitdir-configs`.
//!
//! Ported, byte-for-byte against git 2.55.0:
//!
//!   * **The whole dispatcher.** No arguments →
//!     ``error: need a subcommand`` + usage on stderr, exit 129. Unknown word →
//!     ``error: unknown subcommand: `X'``. Unknown `--long` →
//!     ``error: unknown option `X'``. Unknown `-x` →
//!     ``error: unknown switch `x'``. `-h` (including as the first letter of a
//!     cluster, e.g. `-hx`) → the usage block on **stdout**, exit 129. `--` and
//!     `--end-of-options` terminate option scanning without naming a
//!     subcommand, so both land on ``error: need a subcommand``. The usage
//!     block is `usage: git submodule--helper <command>\n\n` in every case.
//!
//!   * **`gitdir <name>`** — git's `submodule_name_to_gitdir` in its default
//!     shape: `repo_git_path(r, "modules/%s", name)`, i.e. the git directory as
//!     git's own setup resolved it, `/modules/`, then the name verbatim (no
//!     validation: `../evil` and `a/b` pass through unchanged). Wrong argument
//!     count → `usage: git submodule--helper gitdir <name>` on stderr (one
//!     line, no trailing blank), exit 129. The git-directory spelling is
//!     reproduced rather than taken from gitoxide, because git prints the
//!     *relative* `.git` when it discovered the repository by walking up, and
//!     `gix` always hands back an absolute path: `.git` for a repository whose
//!     `.git` is a real directory, the value of `GIT_DIR` verbatim when that is
//!     set, the resolved absolute path for a `.git` gitfile or linked worktree,
//!     and `.` (which `cleanup_path` then elides, yielding `modules/<name>`)
//!     for a bare repository entered at its top level.
//!
//!   * **`get-default-remote <path>`** — git's `repo_get_default_remote` run
//!     against the repository at `<path>`: the branch's `branch.<name>.remote`
//!     when `HEAD` is a symref into `refs/heads/`, otherwise `origin`. A
//!     detached, unborn or remote-less `HEAD` therefore all print `origin`.
//!     A path that is not a repository →
//!     `fatal: could not get a repository handle for submodule '<prefix+path>'`
//!     and exit 128, with the path reported relative to the superproject root
//!     exactly as git's `prefix_path` renders it. Wrong argument count → the
//!     `usage_with_options` block (usage line plus a blank line) on stderr,
//!     exit 129.
//!
//!   * **`status`** and **`init`** delegate to [`super::submodule`], which
//!     implements them. `git submodule--helper status` and `git submodule
//!     status` are the same C function upstream, and were confirmed to emit
//!     identical bytes here (including the `../sm` display path from a
//!     subdirectory).
//!
//! Not ported — each bails naming the missing substrate rather than guessing:
//!
//!   * `clone`, `add`, `update` — need a working clone/fetch/checkout of the
//!     submodule, i.e. transport plus worktree materialisation per submodule.
//!   * `foreach` — runs an arbitrary shell command once per submodule.
//!   * `sync`, `set-url`, `set-branch` — rewrite `.gitmodules` and the remote
//!     urls inside each submodule.
//!   * `deinit`, `absorbgitdirs` — move or delete submodule git dirs and
//!     worktrees.
//!   * `summary` — the submodule log walk and its diff formatting.
//!   * `push-check` — validates the push refspec against the submodule's
//!     remote; needs the refspec/remote machinery.
//!   * `create-branch` — `git branch` inside a submodule with `--track`
//!     bookkeeping.
//!   * `migrate-gitdir-configs` — the `extensions.submodulePathConfig`
//!     migration (rewrites `core.repositoryformatversion`, sets
//!     `submodule.<name>.gitdir` per module, relocates git dirs).
//!
//! `gitdir` additionally bails when `extensions.submodulePathConfig` is
//! enabled: that path reads `submodule.<name>.gitdir` and runs git's
//! `validate_submodule_git_dir` containment check, neither of which any
//! vendored crate under `src/ported` implements. (`gix` may also refuse to open
//! such a repository outright, since the extension is unknown to it.)

use anyhow::{bail, Result};
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::config::KeyRef;

/// The dispatcher's usage block: one line plus a blank line, 40 bytes.
const USAGE: &str = "usage: git submodule--helper <command>\n\n";

/// `git submodule--helper` — dispatch to a submodule subcommand.
///
/// Reproduces `parse_options`' `PARSE_OPT_SUBCOMMAND` behaviour exactly (this
/// builtin declares no options of its own), then routes to the four ported
/// subcommands; every other registered subcommand bails.
pub fn submodule__helper(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands us the tail; tolerate the subcommand name at index 0 so
    // either calling convention behaves the same.
    let args = match args.first() {
        Some(a) if a == "submodule--helper" => &args[1..],
        _ => args,
    };

    let mut sub: Option<usize> = None;
    for (n, a) in args.iter().enumerate() {
        // `--`/`--end-of-options` stop option scanning; parse_options then has
        // no subcommand to run, which is the "need a subcommand" path.
        if a == "--" || a == "--end-of-options" {
            break;
        }
        if let Some(name) = a.strip_prefix("--") {
            eprintln!("error: unknown option `{name}'");
            eprint!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        // `-` alone is not an option; it falls through as a subcommand name.
        if a.len() > 1 && a.starts_with('-') {
            // Short cluster: the first letter decides. `-h` wins immediately
            // (so `-hx` prints help), any other letter is reported and stops.
            let c = a[1..].chars().next().expect("len > 1");
            if c == 'h' {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            eprintln!("error: unknown switch `{c}'");
            eprint!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        sub = Some(n);
        break;
    }

    let Some(n) = sub else {
        eprintln!("error: need a subcommand");
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    };
    let name = args[n].as_str();
    let tail = &args[n + 1..];

    match name {
        "gitdir" => gitdir(tail),
        "get-default-remote" => get_default_remote(tail),
        // Upstream these are literally the same functions `git submodule`
        // dispatches to, so the porcelain module owns the implementation.
        "status" | "init" => {
            let mut forwarded = Vec::with_capacity(tail.len() + 1);
            forwarded.push(name.to_string());
            forwarded.extend(tail.iter().cloned());
            super::submodule::submodule(&forwarded)
        }
        "clone" => bail!(
            "unsupported subcommand \"clone\": cloning a submodule needs transport plus worktree checkout (ported: gitdir, get-default-remote, status, init)"
        ),
        "add" => bail!(
            "unsupported subcommand \"add\": needs a clone of the new submodule (ported: gitdir, get-default-remote, status, init)"
        ),
        "update" => bail!(
            "unsupported subcommand \"update\": needs clone/fetch/checkout of submodules (ported: gitdir, get-default-remote, status, init)"
        ),
        "foreach" => bail!(
            "unsupported subcommand \"foreach\": runs a shell command per submodule (ported: gitdir, get-default-remote, status, init)"
        ),
        "sync" => bail!(
            "unsupported subcommand \"sync\": rewrites remote urls inside submodules (ported: gitdir, get-default-remote, status, init)"
        ),
        "deinit" => bail!(
            "unsupported subcommand \"deinit\": removes submodule worktrees (ported: gitdir, get-default-remote, status, init)"
        ),
        "summary" => bail!(
            "unsupported subcommand \"summary\": needs the submodule log walk (ported: gitdir, get-default-remote, status, init)"
        ),
        "push-check" => bail!(
            "unsupported subcommand \"push-check\": needs the remote/refspec machinery (ported: gitdir, get-default-remote, status, init)"
        ),
        "absorbgitdirs" => bail!(
            "unsupported subcommand \"absorbgitdirs\": relocates submodule git dirs (ported: gitdir, get-default-remote, status, init)"
        ),
        "set-url" => bail!(
            "unsupported subcommand \"set-url\": edits .gitmodules (ported: gitdir, get-default-remote, status, init)"
        ),
        "set-branch" => bail!(
            "unsupported subcommand \"set-branch\": edits .gitmodules (ported: gitdir, get-default-remote, status, init)"
        ),
        "create-branch" => bail!(
            "unsupported subcommand \"create-branch\": creates a branch inside a submodule (ported: gitdir, get-default-remote, status, init)"
        ),
        "migrate-gitdir-configs" => bail!(
            "unsupported subcommand \"migrate-gitdir-configs\": the extensions.submodulePathConfig migration is not ported (ported: gitdir, get-default-remote, status, init)"
        ),
        other => {
            eprintln!("error: unknown subcommand: `{other}'");
            eprint!("{USAGE}");
            Ok(ExitCode::from(129))
        }
    }
}

// ---------------------------------------------------------------- gitdir ----

/// `git submodule--helper gitdir <name>` — print the git directory that the
/// submodule `<name>` uses, i.e. `<git-dir>/modules/<name>`.
fn gitdir(args: &[String]) -> Result<ExitCode> {
    if args.len() != 1 {
        eprintln!("usage: git submodule--helper gitdir <name>");
        return Ok(ExitCode::from(129));
    }
    let name = args[0].as_str();

    let repo = gix::discover(".")?;
    if repo
        .config_snapshot()
        .boolean("extensions.submodulePathConfig")
        .unwrap_or(false)
    {
        bail!(
            "extensions.submodulePathConfig is enabled: resolving `submodule.{name}.gitdir` and \
             git's validate_submodule_git_dir containment check are not ported"
        );
    }

    let mut path = git_dir_spelling(&repo)?;
    if !path.ends_with('/') {
        path.push('/');
    }
    path.push_str("modules/");
    path.push_str(name);
    println!("{}", cleanup_path(&path));
    Ok(ExitCode::SUCCESS)
}

/// How git's own setup would have spelled `$GIT_DIR` for this repository.
///
/// git prints this string verbatim, so the relative forms matter: see the
/// module docs for the four cases reproduced here.
fn git_dir_spelling(repo: &gix::Repository) -> Result<String> {
    // `setup_git_directory` takes `GIT_DIR` as given, without normalising it.
    if let Some(dir) = std::env::var_os("GIT_DIR") {
        let dir = dir.to_string_lossy().into_owned();
        if !dir.is_empty() {
            return Ok(dir);
        }
    }

    let git_dir = repo.git_dir();
    let real_git_dir = std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_owned());

    match repo.workdir() {
        Some(workdir) => {
            // Discovery walked up to a top level whose `.git` is a real
            // directory: git chdir'd there and kept the relative name.
            let dot_git = workdir.join(".git");
            let plain = dot_git.is_dir()
                && std::fs::canonicalize(&dot_git)
                    .map(|p| p == real_git_dir)
                    .unwrap_or(false);
            if plain {
                return Ok(".git".to_string());
            }
            // A `.git` gitfile or a linked worktree: git resolved it to an
            // absolute path before storing it.
            Ok(real_git_dir.to_string_lossy().into_owned())
        }
        None => {
            // Bare: git names it `.` when the cwd *is* the repository.
            let here = std::env::current_dir()
                .ok()
                .and_then(|p| std::fs::canonicalize(p).ok());
            if here.as_deref() == Some(real_git_dir.as_path()) {
                return Ok(".".to_string());
            }
            Ok(real_git_dir.to_string_lossy().into_owned())
        }
    }
}

/// git's `cleanup_path`: drop one leading `./`, then any slashes it left behind.
/// This is what turns `./modules/foo` into `modules/foo` in a bare repository.
fn cleanup_path(path: &str) -> &str {
    match path.strip_prefix("./") {
        Some(rest) => rest.trim_start_matches('/'),
        None => path,
    }
}

// ---------------------------------------------------- get-default-remote ----

/// `git submodule--helper get-default-remote <path>` — print the remote the
/// submodule at `<path>` would fetch from by default.
fn get_default_remote(args: &[String]) -> Result<ExitCode> {
    if args.len() != 1 {
        eprint!("usage: git submodule--helper get-default-remote <path>\n\n");
        return Ok(ExitCode::from(129));
    }
    let path = args[0].as_str();

    // `gix::open` does not walk upwards, matching `repo_submodule_init`, which
    // fails outright when `<path>` is not itself a repository.
    let Ok(sub) = gix::open(path) else {
        let repo = gix::discover(".")?;
        let display = prefixed_path(&repo, path)?;
        eprintln!("fatal: could not get a repository handle for submodule '{display}'");
        return Ok(ExitCode::from(128));
    };

    // `repo_get_default_remote`: a symref into `refs/heads/` consults
    // `branch.<name>.remote`; everything else (detached HEAD) is `origin`.
    let head = sub.head()?;
    let branch = match head.referent_name() {
        Some(name) => {
            let full = name.as_bstr().to_str_lossy().into_owned();
            let Some(short) = full.strip_prefix("refs/heads/") else {
                bail!("HEAD of '{path}' points to {full}, which is not a branch");
            };
            Some(BString::from(short))
        }
        None => None,
    };
    drop(head);

    let remote = branch.and_then(|branch| {
        sub.config_snapshot().string(KeyRef {
            section_name: "branch",
            subsection_name: Some(branch.as_bstr()),
            value_name: "remote",
        })
    });

    match remote {
        Some(remote) => println!("{}", remote.to_str_lossy()),
        None => println!("origin"),
    }
    Ok(ExitCode::SUCCESS)
}

/// git's `prefix_path`: `<path>` re-expressed relative to the repository root
/// by prepending the current prefix and folding `.`/`..` lexically.
fn prefixed_path(repo: &gix::Repository, path: &str) -> Result<String> {
    let prefix = repo
        .prefix()?
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut parts: Vec<&str> = Vec::new();
    for component in prefix
        .split('/')
        .chain(path.split('/'))
        .filter(|c| !c.is_empty() && *c != ".")
    {
        if component == ".." {
            parts.pop();
        } else {
            parts.push(component);
        }
    }
    Ok(parts.join("/"))
}
