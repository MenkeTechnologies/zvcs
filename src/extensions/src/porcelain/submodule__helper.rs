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
//!   * **`status`**, **`init`**, **`foreach`**, **`summary`**, **`sync`**,
//!     **`update`**, **`deinit`**, **`absorbgitdirs`**, **`set-branch`** and
//!     **`set-url`** delegate to [`super::submodule::subcommand`], which
//!     implements them. Each is registered in builtin/submodule--helper.c's
//!     `OPT_SUBCOMMAND` table against the very same C function (`module_status`,
//!     `module_init`, `module_foreach`, `module_summary`, `module_sync`,
//!     `module_update`, `module_deinit`, `absorb_git_dirs`,
//!     `module_set_branch`, `module_set_url`) that `git submodule <name>`
//!     dispatches to, so forwarding `[<name>, <tail>...]` into the porcelain
//!     module reproduces the helper. `status`/`init` were confirmed to emit
//!     identical bytes here (including the `../sm` display path from a
//!     subdirectory).
//!
//!     The forward deliberately targets `subcommand` rather than
//!     `submodule`: the porcelain entry point also reproduces
//!     `git-submodule.sh:29`'s `GIT_PROTOCOL_FROM_USER=0` export, and the helper
//!     has no such export — which is why `git submodule--helper update --remote`
//!     fetches over a `file` url where `git submodule update --remote` dies
//!     `transport 'file' not allowed`.
//!
//!   * **`add`** parses `module_add`'s own option table here — the porcelain
//!     wrapper forwards its arguments unvalidated except for a missing
//!     `<repository>`, so the two disagree on the error: a wrong operand count
//!     is `usage_with_options` (the add usage block, exit 129) for the helper
//!     and the `git-submodule.sh` usage block (exit 1) for `git submodule add`.
//!     Past those checks the work is the porcelain's.
//!
//! Not ported — each bails naming the missing substrate rather than guessing:
//!
//!   * `clone` — needs transport plus worktree materialisation for a submodule
//!     that `update` has not already planned.
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
#[allow(non_snake_case)] // maps to git's `submodule--helper` subcommand
pub fn submodule__helper(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands us the tail; tolerate the subcommand name at index 0 so
    // either calling convention behaves the same.
    let args = match args.first() {
        Some(a) if a == "submodule--helper" => &args[1..],
        _ => args,
    };

    let mut sub: Option<usize> = None;
    // Scans args left-to-right to find the subcommand token; the first hit returns,
    // so clippy sees "loop that never iterates twice" — the scan is intentional.
    #[allow(clippy::never_loop)]
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
        // Upstream these are literally the same C functions `git submodule`
        // dispatches to (`module_status`, `module_init`, `module_foreach`,
        // `module_summary`, `module_sync`, `module_update`, `module_deinit`,
        // `absorb_git_dirs`, `module_set_branch`, `module_set_url` — see the
        // `OPT_SUBCOMMAND` table in builtin/submodule--helper.c), so the
        // porcelain module owns the implementation and the helper forwards to
        // its shared subcommand table. It is deliberately *not* routed through
        // `submodule()`: that entry point also reproduces `git-submodule.sh`'s
        // `GIT_PROTOCOL_FROM_USER=0` export, which the helper does not have —
        // `git submodule--helper update --remote` fetches over `file` where
        // `git submodule update --remote` refuses.
        "status" | "init" | "foreach" | "summary" | "set-branch" | "sync" | "update"
        | "deinit" | "absorbgitdirs" | "set-url" => {
            let mut forwarded = Vec::with_capacity(tail.len() + 1);
            forwarded.push(name.to_string());
            forwarded.extend(tail.iter().cloned());
            super::submodule::subcommand(&forwarded)
        }
        // `module_add` is likewise the same C function `git submodule add`
        // reaches, but the two disagree before it: the porcelain wrapper does no
        // option parsing of its own and forwards everything, so a wrong operand
        // count here is `module_add`'s own `usage_with_options` (exit 129) while
        // the wrapper's is the `git-submodule.sh` usage block (exit 1).
        "add" => add(tail),
        "clone" => anyhow::bail!(
            "unsupported subcommand \"clone\": cloning a submodule needs transport plus worktree checkout (ported: gitdir, get-default-remote, status, init, foreach, summary, sync, update, deinit, absorbgitdirs, set-branch, set-url, add)"
        ),
        "push-check" => anyhow::bail!(
            "unsupported subcommand \"push-check\": needs the remote/refspec machinery (ported: gitdir, get-default-remote, status, init, foreach, summary, sync, update, deinit, absorbgitdirs, set-branch, set-url, add)"
        ),
        "create-branch" => anyhow::bail!(
            "unsupported subcommand \"create-branch\": creates a branch inside a submodule (ported: gitdir, get-default-remote, status, init, foreach, summary, sync, update, deinit, absorbgitdirs, set-branch, set-url, add)"
        ),
        "migrate-gitdir-configs" => bail!(
            "unsupported subcommand \"migrate-gitdir-configs\": the extensions.submodulePathConfig migration is not ported (ported: gitdir, get-default-remote, status, init, foreach, summary, sync, update, deinit, absorbgitdirs, set-branch, set-url, add)"
        ),
        other => {
            eprintln!("error: unknown subcommand: `{other}'");
            eprint!("{USAGE}");
            Ok(ExitCode::from(129))
        }
    }
}

// ------------------------------------------------------------------- add ----

/// The `usage:` block `module_add`'s `usage_with_options` prints, captured
/// byte-for-byte from git 2.55.0 (`git submodule--helper add`): the usage line, a
/// blank line, the nine options with their help text in column 27, and a
/// trailing blank line. Exit is 129.
const ADD_USAGE: &str = "\
usage: git submodule add [<options>] [--] <repository> [<path>]

    -b, --[no-]branch <branch>
                          branch of repository to add as submodule
    -f, --[no-]force      allow adding an otherwise ignored submodule path
    -q, --[no-]quiet      print only error messages
    --[no-]progress       force cloning progress
    --[no-]reference <repository>
                          reference repository
    --[no-]ref-format <format>
                          specify the reference format to use
    --[no-]dissociate     borrow the objects from reference repositories
    --[no-]name <name>    sets the submodule's name to the given string instead of defaulting to its path
    --[no-]depth <n>      depth for shallow clones

";

/// `git submodule--helper add [<options>] [--] <repository> [<path>]` — git's
/// `module_add` (submodule--helper.c:3642).
///
/// Only the two checks `module_add` performs before it starts working are here —
/// the writable-`.gitmodules` gate and the operand count — because they are the
/// two that differ from the porcelain wrapper, which forwards its arguments
/// unvalidated except for a missing `<repository>`. Everything past them is the
/// same C function `git submodule add` reaches, so it is delegated to the shared
/// subcommand table.
fn add(args: &[String]) -> Result<ExitCode> {
    /// `module_add`'s long options that take a value.
    const VALUED: &[&str] = &["branch", "reference", "ref-format", "name", "depth"];
    /// …and the ones that do not.
    const FLAGS: &[&str] = &["force", "quiet", "progress", "dissociate"];

    let mut operands = 0usize;
    let mut end_of_options = false;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        i += 1;
        if end_of_options || !a.starts_with('-') || a == "-" {
            operands += 1;
            continue;
        }
        if a == "--" {
            end_of_options = true;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            // `--no-<name>` unsets; for a valued option it simply clears it.
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            let name = name.strip_prefix("no-").unwrap_or(name);
            if FLAGS.contains(&name) {
                continue;
            }
            if VALUED.contains(&name) {
                if inline.is_none() && !long.starts_with("no-") {
                    if args.get(i).is_none() {
                        eprintln!("error: option `{name}' requires a value");
                        eprint!("{ADD_USAGE}");
                        return Ok(ExitCode::from(129));
                    }
                    i += 1;
                }
                continue;
            }
            eprintln!("error: unknown option `{long}'");
            eprint!("{ADD_USAGE}");
            return Ok(ExitCode::from(129));
        }
        // A short cluster: `-qf`, `-bmain`, `-b main`.
        let mut chars = a[1..].char_indices();
        while let Some((at, c)) = chars.next() {
            match c {
                'f' | 'q' => {}
                'b' => {
                    // The rest of the cluster is the value; an empty rest takes
                    // the next argument.
                    let rest = &a[1 + at + c.len_utf8()..];
                    if rest.is_empty() {
                        if args.get(i).is_none() {
                            eprintln!("error: switch `b' requires a value");
                            eprint!("{ADD_USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                        i += 1;
                    }
                    break;
                }
                other => {
                    eprintln!("error: unknown switch `{other}'");
                    eprint!("{ADD_USAGE}");
                    return Ok(ExitCode::from(129));
                }
            }
        }
    }

    // `is_writing_gitmodules_ok()` runs *before* the operand count: `.gitmodules`
    // must be in the working tree, or absent from the index and HEAD alike.
    if !writing_gitmodules_ok()? {
        crate::git_fatal!("please make sure that the .gitmodules file is in the working tree");
    }

    if operands == 0 || operands > 2 {
        eprint!("{ADD_USAGE}");
        return Ok(ExitCode::from(129));
    }

    let mut forwarded = Vec::with_capacity(args.len() + 1);
    forwarded.push("add".to_string());
    forwarded.extend(args.iter().cloned());
    super::submodule::subcommand(&forwarded)
}

/// git's `is_writing_gitmodules_ok` (submodule.c): the worktree copy exists, or
/// there is no `.gitmodules` in the index nor in `HEAD` to be shadowed by one.
fn writing_gitmodules_ok() -> Result<bool> {
    let repo = gix::discover(".")?;
    if let Some(workdir) = repo.workdir() {
        if workdir.join(".gitmodules").exists() {
            return Ok(true);
        }
    }
    let path = BString::from(".gitmodules");
    let in_index = repo
        .index_or_empty()?
        .entry_by_path(path.as_bstr())
        .is_some();
    let in_head = repo
        .head_commit()
        .ok()
        .and_then(|c| c.tree().ok())
        .and_then(|t| t.lookup_entry_by_path(".gitmodules").ok().flatten())
        .is_some();
    Ok(!in_index && !in_head)
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
                crate::git_fatal!("HEAD of '{path}' points to {full}, which is not a branch");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the helper's two usage blocks to the bytes git 2.55.0 writes.
    ///
    /// `ADD_USAGE` is `module_add`'s `usage_with_options` output, captured from
    /// `git submodule--helper add` on git 2.55.0. Its shape is the part that
    /// silently rots: `parse_options` puts help text in column 27 and spills an
    /// option whose `-x, --[no-]name <arg>` header already reaches that column
    /// onto its own line — which is why `branch`, `reference`, `ref-format` are
    /// two-line entries and `force`, `quiet`, `progress`, `dissociate`, `name`,
    /// `depth` are one.
    #[test]
    fn usage_blocks_match_git() {
        assert_eq!(USAGE, "usage: git submodule--helper <command>\n\n");

        let lines: Vec<&str> = ADD_USAGE.split('\n').collect();
        assert_eq!(
            lines[0],
            "usage: git submodule add [<options>] [--] <repository> [<path>]"
        );
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "    -b, --[no-]branch <branch>");
        assert_eq!(
            lines[3],
            "                          branch of repository to add as submodule"
        );
        assert_eq!(
            lines[4],
            "    -f, --[no-]force      allow adding an otherwise ignored submodule path"
        );
        assert_eq!(
            lines[5],
            "    -q, --[no-]quiet      print only error messages"
        );
        assert_eq!(
            *lines.last().expect("non-empty"),
            "",
            "parse_options ends the block with a blank line"
        );
        assert!(ADD_USAGE.ends_with("depth for shallow clones\n\n"));
        // Every wrapped help line is indented to the same column as the inline
        // ones, so a mis-measured pad would show up as a mismatch here.
        for line in ADD_USAGE
            .lines()
            .filter(|l| l.starts_with("                "))
        {
            assert_eq!(
                line.len() - line.trim_start().len(),
                26,
                "help column drifted: {line:?}"
            );
        }
    }
}
