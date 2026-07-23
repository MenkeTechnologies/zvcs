use anyhow::{bail, Result};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::commit::describe::SelectRef;
use gix::config::{File as ConfigFile, KeyRef, Source};
use gix::hash::ObjectId;
use gix::prelude::ObjectIdExt;

/// The exact `usage:` block stock `git submodule` prints on any parse error,
/// for every subcommand. git emits it on stderr and exits 1 (not 129).
const USAGE: &str = "\
usage: git submodule [--quiet] [--cached]
   or: git submodule [--quiet] add [-b <branch>] [-f|--force] [--name <name>] [--reference <repository>] [--] <repository> [<path>]
   or: git submodule [--quiet] status [--cached] [--recursive] [--] [<path>...]
   or: git submodule [--quiet] init [--] [<path>...]
   or: git submodule [--quiet] deinit [-f|--force] (--all| [--] <path>...)
   or: git submodule [--quiet] update [--init [--filter=<filter-spec>]] [--remote] [-N|--no-fetch] [-f|--force] [--checkout|--merge|--rebase] [--[no-]recommend-shallow] [--reference <repository>] [--recursive] [--[no-]single-branch] [--] [<path>...]
   or: git submodule [--quiet] set-branch (--default|--branch <branch>) [--] <path>
   or: git submodule [--quiet] set-url [--] <path> <newurl>
   or: git submodule [--quiet] summary [--cached|--files] [--summary-limit <n>] [commit] [--] [<path>...]
   or: git submodule [--quiet] foreach [--recursive] <command>
   or: git submodule [--quiet] sync [--recursive] [--] [<path>...]
   or: git submodule [--quiet] absorbgitdirs [--] [<path>...]
";

/// Print the usage block and hand back git's exit code for a parse error.
fn usage_exit() -> Result<ExitCode> {
    eprint!("{USAGE}");
    Ok(ExitCode::from(1))
}

/// git rejects an empty pathspec while parsing one, before any listing happens.
fn reject_empty_pathspec(patterns: &[BString]) -> Option<ExitCode> {
    if patterns.iter().any(|p| p.is_empty()) {
        eprintln!(
            "fatal: empty string is not a valid pathspec. please use . instead if you meant to match all paths"
        );
        return Some(ExitCode::from(128));
    }
    None
}

/// The subcommand names stock git recognizes after the global flags. Anything
/// else — including `--`, a stray option, or a path — is a usage error.
const SUBCOMMANDS: &[&str] = &[
    "add",
    "foreach",
    "init",
    "deinit",
    "update",
    "set-branch",
    "set-url",
    "summary",
    "status",
    "sync",
    "absorbgitdirs",
];

/// `git submodule` — inspect and register the submodules recorded in the index.
///
/// Seven of the eleven stock subcommands are ported here, all aiming at
/// byte-for-byte parity with stock `git`:
///
///   * `git submodule [--quiet] [--cached] [status] [--recursive] [--] [<path>...]`
///     The default subcommand. Enumerates gitlink entries of the index (not of
///     `.gitmodules`), cross-references each against `.gitmodules`, and prints
///     `<state><oid> <displaypath> (<rev-name>)`. `<state>` is `U` for a
///     conflicted entry, `-` when the submodule is inactive or has no
///     repository, `+` when its `HEAD` differs from the superproject's index,
///     and a space otherwise. `--cached` prints (and names) the index oid even
///     in the `+` case. Display paths are relative to the current directory,
///     matching git's `get_submodule_displaypath`.
///
///   * `git submodule [--quiet] init [--] [<path>...]`
///     Registers `submodule.<name>.active`, `submodule.<name>.url` and (when
///     `.gitmodules` carries one and the config does not) `submodule.<name>.update`
///     into the repository-local config, printing (to stderr, as git does)
///     `Submodule '<name>' (<url>) registered for path '<path>'` per newly
///     registered url.
///
///   * `git submodule [--quiet] summary [--cached|--files] [--summary-limit <n>]
///     [<commit>] [--] [<path>...]`
///     The gitlink half of `git diff-index`/`git diff-files`, rendered as
///     `* <displaypath> <src>...<dst> (<n>):` followed by one `  > <subject>` or
///     `  < <subject>` line per commit in the first-parent symmetric difference
///     and a blank line. Like git, an unpopulated submodule contributes nothing.
///
///   * `git submodule [--quiet] foreach [--recursive] <command>`
///     Runs `<command>` through `sh` inside each populated submodule with
///     `name`, `sm_path`, `displaypath`, `sha1` and `toplevel` exported, printing
///     `Entering '<displaypath>'` first unless quiet. A failing command aborts
///     the walk with git's `run_command returned non-zero status` fatal and 128.
///
///   * `git submodule [--quiet] sync [--recursive] [--] [<path>...]`
///     Re-copies each active submodule's `.gitmodules` url into the superproject
///     `submodule.<name>.url`, and — when the submodule is populated — rewrites
///     `remote.<default-remote>.url` inside the submodule's own config, where the
///     default remote is `branch.<current>.remote` (else `origin`). Prints
///     `Synchronizing submodule url for '<displaypath>'` per active submodule.
///     A relative (`./`, `../`) url bails: `resolve_relative_url` is not ported.
///
///   * `git submodule [--quiet] update [--init] [--remote] [-N|--no-fetch]
///     [-f|--force] [--checkout|--merge|--rebase] [--recursive] [--] [<path>...]`
///     Brings each submodule to the commit the superproject records — checked out
///     on a detached HEAD, or merged/rebased into the submodule branch under
///     `--merge`/`--rebase` (or a `submodule.<name>.update` of `merge`/`rebase`) —
///     fetching it in first (via the vendored gix blocking transport, re-executed
///     as a child `fetch`) when it is not already reachable. `--remote` retargets
///     to the tip of the submodule's remote-tracking branch, fetched fresh. A
///     not-yet-populated submodule is cloned by re-executing the ported `clone`
///     against its registered `submodule.<name>.url`, then checked out. `--init`
///     first runs the same registration pass as `init`; `--recursive` descends.
///     Each non-checkout step is a re-exec of the matching ported subcommand
///     (`merge`/`rebase`/`clone`), so the whole `git-submodule.sh` update path is
///     covered except the pieces that are not a ported-command re-exec: a relative
///     `.gitmodules` url (`resolve_relative_url`), the clone/fetch-shaping flags
///     (`--depth`, `--reference`, `--dissociate`, `--recommend-shallow`,
///     `--single-branch`, `--filter`, `--require-init`), and a `!command` update
///     strategy — each of which bails.
///
///   * `git submodule [--quiet] set-branch (--default|--branch <branch>) [--] <path>`
///     Writes (or, under `--default`, removes) `submodule.<name>.branch` in the
///     worktree `.gitmodules`, keyed by the submodule *name* resolved from `<path>`
///     through the `.gitmodules` mapping. Matches stock git 2.55 (the installed
///     `git`, newer than the v2.39 spec whose helper still keyed by raw `<path>`):
///     an unmatched `<path>` dies with `no submodule mapping found in .gitmodules
///     for path '<path>'` (128); giving neither or both of `--branch`/`--default`
///     dies 128; a wrong operand count prints the set-branch usage and exits 129;
///     `--default` exits 0 when it removed a branch key and 1 when there was none.
///
/// `--quiet` is accepted in front of every subcommand, but `--cached` is only
/// declared by `status` and `summary`, so a leading `--cached` in front of any
/// other subcommand is a usage error exiting 1 — `--quiet` does not suppress the
/// usage block. A bare `git submodule --cached` is valid, resolving to `status`.
///
/// The `<rev-name>` suffix is git's `compute_rev_name`, which shells out to
/// `git describe` four times in order: bare, `--tags`, `--contains`, and
/// `--all --always`. Stages 1, 2 and 4 are backed by gitoxide's describe
/// implementation — stage 4 through the plumbing entry point with a name table
/// built exactly like git's `get_name()` under `--all` (full ref names minus
/// `refs/`), which the `gix` convenience selector does not produce as it
/// shortens names instead. Stage 3 is `git name-rev`, a distinct algorithm that
/// is not part of the vendored crates, so when stages 1 and 2 find nothing
/// while the submodule does hold tags, this bails rather than skipping ahead to
/// stage 4 and printing a name git would not have printed.
///
/// Not ported, each rejected with a precise reason rather than approximated:
/// `add`, `deinit`, `set-url` and `absorbgitdirs`. `init` (and `sync`)
/// additionally bail on `.gitmodules` urls that are
/// relative (`./`, `../`), which require git's `resolve_relative_url` against
/// the superproject's default remote.
pub fn submodule(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands us the subcommand at index 0; tolerate both conventions so
    // the wiring may pass either `["submodule", ...]` or just the tail.
    let args = match args.first() {
        Some(a) if a == "submodule" => &args[1..],
        _ => args,
    };

    // `git submodule [--quiet] [--cached] [<subcommand>]` — the two global flags
    // may precede the subcommand, and mean the same as passing them after it.
    let mut quiet = false;
    let mut cached = false;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "-q" | "--quiet" => quiet = true,
            "--cached" => cached = true,
            _ => break,
        }
        i += 1;
    }

    // git's top level takes flags and a subcommand name and nothing else: a
    // leftover option (`--recursive`), a `--`, or a path all reach
    // `usage_with_options`, which prints the usage block and exits 1.
    let (name, tail) = match args.get(i) {
        None => ("status", &args[i..]),
        Some(a) if SUBCOMMANDS.contains(&a.as_str()) => (a.as_str(), &args[i + 1..]),
        Some(_) => return usage_exit(),
    };

    // Only `status` and `summary` declare `--cached` in their option parsers, so
    // a global `--cached` in front of any other subcommand falls through to
    // `usage_with_options`. This is checked before the subcommand's own argument
    // parsing runs: `git submodule --cached foreach` prints the usage block
    // rather than foreach's missing-<command> error. A bare `git submodule
    // --cached` is fine — it resolves to `status`, which accepts the flag.
    if cached && !matches!(name, "status" | "summary") {
        return usage_exit();
    }

    match name {
        "status" => status(tail, quiet, cached),
        "init" => init(tail, quiet),
        "summary" => summary(tail, cached),
        "foreach" => foreach(tail, quiet),
        "add" => bail!("`submodule add` needs a clone of the new submodule; not ported"),
        "update" => update(tail, quiet),
        "deinit" => bail!("`submodule deinit` removes submodule worktrees; not ported"),
        "sync" => sync(tail, quiet),
        "set-branch" => set_branch(tail, quiet),
        "set-url" => bail!("`submodule set-url` edits .gitmodules; not ported"),
        "absorbgitdirs" => bail!("`submodule absorbgitdirs` relocates git dirs; not ported"),
        _ => usage_exit(),
    }
}

/// One gitlink entry of the index, as git's `module_list_compute` yields it.
struct Entry {
    /// Repository-root-relative path of the submodule.
    path: BString,
    /// The object id recorded in the superproject's index.
    oid: ObjectId,
    /// True when the entry sits at a merge stage other than 0.
    conflicted: bool,
}

// ---------------------------------------------------------------- status ----

fn status(args: &[String], mut quiet: bool, mut cached: bool) -> Result<ExitCode> {
    let mut patterns: Vec<BString> = Vec::new();
    let mut no_more_opts = false;
    let mut recursive = false;

    for a in args {
        if no_more_opts {
            patterns.push(BString::from(a.as_str()));
            continue;
        }
        match a.as_str() {
            "--" => no_more_opts = true,
            "-q" | "--quiet" => quiet = true,
            "--cached" => cached = true,
            "--recursive" => recursive = true,
            _ if a.starts_with('-') => return usage_exit(),
            // git parses `status` with `PARSE_OPT_STOP_AT_NON_OPTION`: the first
            // non-option operand ends option parsing, so a later `--recursive`
            // (or any dash-prefixed token) is a pathspec, not a flag.
            _ => {
                patterns.push(BString::from(a.as_str()));
                no_more_opts = true;
            }
        }
    }

    if let Some(code) = reject_empty_pathspec(&patterns) {
        return Ok(code);
    }

    let repo = gix::discover(".")?;
    let prefix = repo_prefix(&repo)?;

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let code = status_repo(
        &mut out,
        &repo,
        &patterns,
        prefix.as_ref(),
        None,
        quiet,
        cached,
        recursive,
    )?;
    out.flush()?;
    Ok(ExitCode::from(code))
}

/// One superproject's worth of `git submodule status`. `super_prefix` is set for
/// every level below the first and already carries its trailing `/`, matching
/// git's `--super-prefix` display paths; at the top level the cwd-relative
/// `prefix` is used instead.
#[allow(clippy::too_many_arguments)]
fn status_repo(
    out: &mut impl Write,
    repo: &gix::Repository,
    patterns: &[BString],
    prefix: Option<&BString>,
    super_prefix: Option<&str>,
    quiet: bool,
    cached: bool,
    recursive: bool,
) -> Result<u8> {
    let index = repo.open_index()?;
    let entries = match module_list(repo, &index, patterns)? {
        Ok(entries) => entries,
        // Unmatched pathspecs: git reports each one and exits 1.
        Err(code) => return Ok(code),
    };

    let submodules = submodules(repo)?;
    let workdir = repo.workdir().map(ToOwned::to_owned);
    let null = ObjectId::null(repo.object_hash());

    for entry in &entries {
        let Some(sub) = find_submodule(&submodules, &entry.path) else {
            out.flush()?;
            eprintln!(
                "fatal: no submodule mapping found in .gitmodules for path '{}'",
                entry.path
            );
            return Ok(128);
        };
        let display = match super_prefix {
            Some(sp) => format!("{sp}{}", entry.path),
            None => display_path(entry.path.as_bstr(), prefix),
        };

        if entry.conflicted {
            print_status(out, quiet, 'U', &null, &display, None)?;
            continue;
        }

        // git prints `-` when the submodule is not active, or when `<path>/.git`
        // does not resolve to a git directory.
        let sub_repo = match workdir.as_ref() {
            Some(wd) => gix::open(wd.join(&*gix::path::from_bstr(entry.path.as_bstr()))).ok(),
            None => None,
        };
        let active = sub.is_active()?;
        let (Some(sub_repo), true) = (sub_repo, active) else {
            print_status(out, quiet, '-', &entry.oid, &display, None)?;
            continue;
        };

        let Ok(head) = sub_repo.head_id() else {
            bail!(
                "submodule '{}' has an unborn HEAD; git's null-oid reporting for that case is not ported",
                entry.path
            );
        };
        let head = head.detach();

        // `git diff-files --ignore-submodules=dirty -- <path>` reduces to "does
        // the submodule's HEAD match what the superproject recorded in its index".
        let state = if head == entry.oid { ' ' } else { '+' };
        let shown = if state == '+' && !cached { head } else { entry.oid };
        let name = rev_name(&sub_repo, &shown)?;
        print_status(out, quiet, state, &shown, &display, name.as_deref())?;

        // `--recursive` descends with the display path as the new super-prefix
        // and no pathspecs, exactly as git re-invokes the helper per level.
        if recursive {
            let nested = format!("{display}/");
            let code = status_repo(
                out,
                &sub_repo,
                &[],
                None,
                Some(&nested),
                quiet,
                cached,
                true,
            )?;
            if code != 0 {
                return Ok(code);
            }
        }
    }

    Ok(0)
}

/// git's `print_status`: `<state><oid> <displaypath>` plus ` (<rev-name>)` when
/// a name was computed (never for the `-` and `U` states).
fn print_status(
    out: &mut impl Write,
    quiet: bool,
    state: char,
    oid: &ObjectId,
    display: &str,
    name: Option<&str>,
) -> Result<()> {
    if quiet {
        return Ok(());
    }
    write!(out, "{state}{} {display}", oid.to_hex())?;
    if let Some(name) = name {
        write!(out, " ({name})")?;
    }
    writeln!(out)?;
    Ok(())
}

// ------------------------------------------------------------------ init ----

fn init(args: &[String], mut quiet: bool) -> Result<ExitCode> {
    let mut patterns: Vec<BString> = Vec::new();
    let mut no_more_opts = false;

    for a in args {
        if no_more_opts {
            patterns.push(BString::from(a.as_str()));
            continue;
        }
        match a.as_str() {
            "--" => no_more_opts = true,
            "-q" | "--quiet" => quiet = true,
            _ if a.starts_with('-') => return usage_exit(),
            // `PARSE_OPT_STOP_AT_NON_OPTION`: the first operand ends option
            // parsing, so trailing dash-prefixed tokens are pathspecs.
            _ => {
                patterns.push(BString::from(a.as_str()));
                no_more_opts = true;
            }
        }
    }

    if let Some(code) = reject_empty_pathspec(&patterns) {
        return Ok(code);
    }

    let repo = gix::discover(".")?;
    Ok(ExitCode::from(init_repo(&repo, &patterns, quiet)?))
}

/// The body of `git submodule init` for one already-opened superproject:
/// register `submodule.<name>.active`/`.url`/`.update` for every listed gitlink.
/// Returns git's exit code (0 on success, 128 for a `.gitmodules` with no url).
/// Factored out of `init` so `update --init` can run the same registration pass
/// against the repository it opened, mirroring git's `module_update` calling the
/// init pass before `update_submodules`.
fn init_repo(repo: &gix::Repository, patterns: &[BString], quiet: bool) -> Result<u8> {
    let index = repo.open_index()?;

    let mut entries = match module_list(repo, &index, patterns)? {
        Ok(entries) => entries,
        Err(code) => return Ok(code),
    };

    let submodules = submodules(repo)?;
    let prefix = repo_prefix(repo)?;

    // With no pathspec and `submodule.active` configured, git restricts the list
    // to the active submodules (`module_list_active`).
    let has_active_config = repo.config_snapshot().string("submodule.active").is_some();
    if patterns.is_empty() && has_active_config {
        let mut kept = Vec::new();
        for entry in entries {
            let active = match find_submodule(&submodules, &entry.path) {
                Some(sub) => sub.is_active()?,
                None => false,
            };
            if active {
                kept.push(entry);
            }
        }
        entries = kept;
    }

    // `.gitmodules` is re-read raw so urls are registered verbatim the way git
    // copies them, rather than round-tripped through a parsed URL type.
    let modules_path = match repo.workdir() {
        Some(wd) => wd.join(".gitmodules"),
        None => std::path::PathBuf::from(".gitmodules"),
    };
    let modules = ConfigFile::from_path_no_includes(modules_path, Source::Local).ok();

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let config_path = repo.common_dir().join("config");
    let mut config = ConfigFile::from_path_no_includes(config_path.clone(), Source::Local)?;
    let mut dirty = false;
    let mut messages: Vec<String> = Vec::new();

    for entry in &entries {
        let display = display_path(entry.path.as_bstr(), prefix.as_ref());
        let Some(sub) = find_submodule(&submodules, &entry.path) else {
            eprintln!("fatal: No url found for submodule path '{display}' in .gitmodules");
            return Ok(128);
        };
        let sub_name = sub.name().to_owned();
        let sub_name = sub_name.as_bstr();

        // Mark it active first — that is the order git writes the two keys in.
        if !sub.is_active()? {
            config.set_raw_value_by("submodule", Some(sub_name), "active", "true")?;
            dirty = true;
        }

        // Reads go against the merged snapshot, matching git's `git_config_get_string`.
        let registered_url = repo.config_snapshot().string(key(sub_name, "url"));
        if registered_url.is_none() {
            let url = modules
                .as_ref()
                .and_then(|m| m.string_by("submodule", Some(sub_name), "url"))
                .filter(|u| !u.is_empty());
            let Some(url) = url else {
                eprintln!("fatal: No url found for submodule path '{display}' in .gitmodules");
                return Ok(128);
            };
            if url.starts_with(b"./") || url.starts_with(b"../") {
                bail!(
                    "submodule '{sub_name}' has the relative url {:?}; resolving it against the default remote is not ported",
                    url.to_str_lossy()
                );
            }
            config.set_raw_value_by("submodule", Some(sub_name), "url", url.as_bstr())?;
            dirty = true;
            if !quiet {
                messages.push(format!(
                    "Submodule '{sub_name}' ({}) registered for path '{display}'",
                    url.to_str_lossy()
                ));
            }
        }

        // Copy the `update` strategy over, but only when the config has none.
        let registered_update = repo.config_snapshot().string(key(sub_name, "update"));
        if registered_update.is_none() {
            if let Some(upd) = modules
                .as_ref()
                .and_then(|m| m.string_by("submodule", Some(sub_name), "update"))
            {
                let upd = upd.to_str_lossy().into_owned();
                match upd.as_str() {
                    "checkout" | "rebase" | "merge" | "none" => {
                        config.set_raw_value_by(
                            "submodule",
                            Some(sub_name),
                            "update",
                            upd.as_str(),
                        )?;
                        dirty = true;
                    }
                    _ if upd.starts_with('!') => bail!(
                        "submodule '{sub_name}' configures `update = {upd}`; git's !command downgrade path is not ported"
                    ),
                    _ => bail!("submodule '{sub_name}' has an unknown update strategy {upd:?}"),
                }
            }
        }
    }

    if dirty {
        persist(&config_path, &config)?;
    }
    // git's `init_submodule` prints this line to stderr (verified against git
    // 2.55.0: `git submodule init 1>out 2>err` leaves `out` empty), so the port
    // must too, or a caller redirecting stdout loses parity.
    for line in messages {
        eprintln!("{line}");
    }
    Ok(0)
}

// --------------------------------------------------------------- summary ----

/// One gitlink row of the diff `git submodule summary` renders, with `None`
/// standing for "this side has no gitlink at that path".
struct Change {
    path: BString,
    src: Option<ObjectId>,
    dst: Option<ObjectId>,
}

fn summary(args: &[String], mut cached: bool) -> Result<ExitCode> {
    let mut files = false;
    // git's `summary_limit` defaults to -1, meaning "no limit"; 0 means
    // "print nothing at all" and short-circuits before any diff is computed.
    let mut limit: i64 = -1;
    let mut rest: Vec<String> = Vec::new();
    let mut no_more_opts = false;

    let mut i = 0;
    while let Some(a) = args.get(i) {
        i += 1;
        if no_more_opts {
            rest.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => no_more_opts = true,
            "--cached" => cached = true,
            "--files" => files = true,
            // `--for-status` only changes the header `git status` prints around
            // this output, never the rows themselves.
            "--for-status" => {}
            "-q" | "--quiet" => {}
            "-n" | "--summary-limit" => match args.get(i).and_then(|v| v.parse::<i64>().ok()) {
                Some(v) => {
                    limit = v;
                    i += 1;
                }
                None => return usage_exit(),
            },
            s if s.starts_with("--summary-limit=") => {
                match s["--summary-limit=".len()..].parse::<i64>() {
                    Ok(v) => limit = v,
                    Err(_) => return usage_exit(),
                }
            }
            s if s.starts_with('-') && s.len() > 1 => return usage_exit(),
            // `PARSE_OPT_STOP_AT_NON_OPTION`: the first operand (the `[commit]`
            // slot, then pathspecs) ends option parsing, so a trailing
            // `--recursive`/`--files`/etc. is an operand rather than a flag.
            // This is why `summary foreach --recursive` exits 0 in git.
            _ => {
                rest.push(a.clone());
                no_more_opts = true;
            }
        }
    }

    if cached && files {
        eprintln!("fatal: options '--cached' and '--files' cannot be used together");
        return Ok(ExitCode::from(128));
    }
    if limit == 0 {
        return Ok(ExitCode::SUCCESS);
    }

    let repo = gix::discover(".")?;
    let index = repo.open_index()?;

    // The first leftover argument is the base revision when it resolves to one,
    // and a pathspec otherwise — git's `repo_get_oid(argv[0])` fallthrough.
    let mut rev: Option<ObjectId> = None;
    if !files {
        if let Some(first) = rest.first() {
            if let Some(id) = resolve_commit(&repo, first.as_str()) {
                rev = Some(id);
                rest.remove(0);
            }
        }
        if rev.is_none() {
            rev = repo.head_id().ok().map(|id| id.detach());
        }
    }

    let patterns: Vec<BString> = rest.iter().map(|s| BString::from(s.as_str())).collect();
    if let Some(code) = reject_empty_pathspec(&patterns) {
        return Ok(code);
    }
    let changes = summary_changes(&repo, &index, rev.as_ref(), files, cached, &patterns)?;

    let prefix = repo_prefix(&repo)?;
    let workdir = repo.workdir().map(ToOwned::to_owned);
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    for change in &changes {
        let display = display_path(change.path.as_bstr(), prefix.as_ref());
        let sub_repo = match workdir.as_ref() {
            Some(wd) => gix::open(wd.join(&*gix::path::from_bstr(change.path.as_bstr()))).ok(),
            None => None,
        };
        // git renders nothing for a submodule it cannot walk: an unpopulated
        // worktree contributes no rows to the summary at all.
        let Some(sub_repo) = sub_repo else { continue };
        print_summary(&mut out, &sub_repo, change, &display, limit)?;
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// `git rev-parse --verify <spec>^{commit}`, reduced to "did it name a commit".
fn resolve_commit(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let id = repo.rev_parse_single(spec.as_bytes().as_bstr()).ok()?;
    let obj = id.object().ok()?;
    obj.peel_to_commit().ok().map(|c| c.id)
}

/// The gitlink rows of `git diff-index [--cached] <rev>` (or `git diff-files`
/// under `--files`), restricted to paths that differ between the two sides.
fn summary_changes(
    repo: &gix::Repository,
    index: &gix::index::State,
    rev: Option<&ObjectId>,
    files: bool,
    cached: bool,
    patterns: &[BString],
) -> Result<Vec<Change>> {
    // Left side: the index under `--files`, the revision's tree otherwise.
    let mut src: HashMap<BString, ObjectId> = HashMap::new();
    if files {
        gitlinks_of_index(index, &mut src);
    } else if let Some(rev) = rev {
        gitlinks_of_tree(repo, rev, &mut src)?;
    }

    // Right side: the index when comparing against it, else the worktree, where
    // a gitlink's content is the submodule's own HEAD.
    let mut dst: HashMap<BString, ObjectId> = HashMap::new();
    gitlinks_of_index(index, &mut dst);
    if files || !cached {
        let workdir = repo.workdir().map(ToOwned::to_owned);
        if let Some(wd) = workdir {
            for (path, oid) in dst.iter_mut() {
                let sm = wd.join(&*gix::path::from_bstr(path.as_bstr()));
                // Detach inside the closure: `head_id` borrows the repository,
                // which is owned by the closure and dropped on return.
                if let Some(head) = gix::open(sm)
                    .ok()
                    .and_then(|r| r.head_id().ok().map(|id| id.detach()))
                {
                    *oid = head;
                }
            }
        }
    }

    let mut paths: Vec<BString> = src.keys().chain(dst.keys()).cloned().collect();
    paths.sort();
    paths.dedup();

    let mut ps = repo.pathspec(
        false,
        patterns,
        false,
        index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;

    let mut changes = Vec::new();
    for path in paths {
        let (s, d) = (src.get(&path).copied(), dst.get(&path).copied());
        if s == d {
            continue;
        }
        if !patterns.is_empty() && !ps.is_included(path.as_bstr(), Some(false)) {
            continue;
        }
        changes.push(Change {
            path,
            src: s,
            dst: d,
        });
    }
    Ok(changes)
}

/// Every stage-0 gitlink of the index, keyed by path.
fn gitlinks_of_index(index: &gix::index::State, into: &mut HashMap<BString, ObjectId>) {
    for entry in index.entries() {
        if entry.mode == gix::index::entry::Mode::COMMIT && entry.stage_raw() == 0 {
            into.insert(entry.path(index).to_owned(), entry.id);
        }
    }
}

/// Every gitlink reachable from the commit `rev`, keyed by its full path.
fn gitlinks_of_tree(
    repo: &gix::Repository,
    rev: &ObjectId,
    into: &mut HashMap<BString, ObjectId>,
) -> Result<()> {
    let Ok(obj) = repo.find_object(*rev) else {
        return Ok(());
    };
    let Ok(commit) = obj.peel_to_commit() else {
        return Ok(());
    };
    let tree = commit.tree()?;
    for entry in tree.traverse().breadthfirst.files()? {
        if entry.mode.is_commit() {
            into.insert(entry.filepath, entry.oid);
        }
    }
    Ok(())
}

/// git's `print_submodule_summary`: the `* <path> <src>...<dst> (<n>):` header,
/// the marked one-line log of the first-parent symmetric difference, and the
/// blank line that separates one submodule from the next.
fn print_summary(
    out: &mut impl Write,
    sub_repo: &gix::Repository,
    change: &Change,
    display: &str,
    limit: i64,
) -> Result<()> {
    // git renders an absent side as seven zeros regardless of `core.abbrev`, and
    // drops both the count and the log when the destination is gone entirely.
    let zeros = "0".repeat(7);
    let abbrev = |oid: &ObjectId| -> String {
        match (*oid).attach(sub_repo).shorten() {
            Ok(prefix) => prefix.to_string(),
            Err(_) => oid.to_hex_with_len(7).to_string(),
        }
    };

    match (change.src, change.dst) {
        // Modified: both sides name a commit.
        (Some(src), Some(dst)) => {
            let (left, right) = first_parent_difference(sub_repo, &src, &dst);
            writeln!(
                out,
                "* {display} {}...{} ({}):",
                abbrev(&src),
                abbrev(&dst),
                left.len() + right.len()
            )?;
            let mut lines: Vec<(i64, char, String)> = Vec::new();
            for (ids, mark) in [(&left, '<'), (&right, '>')] {
                for id in ids {
                    if let Some((time, subject)) = commit_summary(sub_repo, id) {
                        lines.push((time, mark, subject));
                    }
                }
            }
            // `git log` walks in reverse chronological order across both sides.
            lines.sort_by(|a, b| b.0.cmp(&a.0));
            let take = if limit > 0 {
                lines.len().min(limit as usize)
            } else {
                lines.len()
            };
            for (_, mark, subject) in &lines[..take] {
                writeln!(out, "  {mark} {subject}")?;
            }
        }
        // Added: git counts the whole first-parent history but logs only the tip.
        (None, Some(dst)) => {
            let total = first_parent_chain(sub_repo, &dst).len();
            writeln!(out, "* {display} {zeros}...{} ({total}):", abbrev(&dst))?;
            if let Some((_, subject)) = commit_summary(sub_repo, &dst) {
                writeln!(out, "  > {subject}")?;
            }
        }
        // Deleted: no count and no log, and the surviving side is not abbreviated
        // against the submodule's object database.
        (Some(src), None) => {
            writeln!(out, "* {display} {}...{zeros}:", src.to_hex_with_len(7))?;
        }
        (None, None) => return Ok(()),
    }
    writeln!(out)?;
    Ok(())
}

/// The committer timestamp and subject line of `id`, or `None` when the object
/// is missing from the submodule's object database.
fn commit_summary(repo: &gix::Repository, id: &ObjectId) -> Option<(i64, String)> {
    let commit = repo.find_object(*id).ok()?.peel_to_commit().ok()?;
    let time = commit.time().map(|t| t.seconds).unwrap_or(0);
    let subject = commit.message().ok()?.summary().to_str_lossy().into_owned();
    Some((time, subject))
}

/// `git rev-list --first-parent <src>...<dst>`, split into the two sides so each
/// commit can carry the `<`/`>` mark `%m` would have printed for it.
fn first_parent_difference(
    repo: &gix::Repository,
    src: &ObjectId,
    dst: &ObjectId,
) -> (Vec<ObjectId>, Vec<ObjectId>) {
    let a = first_parent_chain(repo, src);
    let b = first_parent_chain(repo, dst);
    let a_set: std::collections::HashSet<&ObjectId> = a.iter().collect();
    let b_set: std::collections::HashSet<&ObjectId> = b.iter().collect();
    // First-parent chains share a tail once they meet, so the unique part of
    // each is its prefix up to the first commit the other side also holds.
    let left: Vec<ObjectId> = a
        .iter()
        .take_while(|id| !b_set.contains(id))
        .copied()
        .collect();
    let right: Vec<ObjectId> = b
        .iter()
        .take_while(|id| !a_set.contains(id))
        .copied()
        .collect();
    (left, right)
}

/// The first-parent ancestry of `tip`, newest first.
fn first_parent_chain(repo: &gix::Repository, tip: &ObjectId) -> Vec<ObjectId> {
    let mut chain = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut next = Some(*tip);
    while let Some(id) = next {
        if !seen.insert(id) {
            break;
        }
        chain.push(id);
        next = repo
            .find_object(id)
            .ok()
            .and_then(|o| o.peel_to_commit().ok())
            .and_then(|c| c.parent_ids().next().map(|p| p.detach()));
    }
    chain
}

// --------------------------------------------------------------- foreach ----

fn foreach(args: &[String], mut quiet: bool) -> Result<ExitCode> {
    let mut recursive = false;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "-q" | "--quiet" => quiet = true,
            "--recursive" => recursive = true,
            "--" => {
                i += 1;
                break;
            }
            s if s.starts_with('-') && s.len() > 1 => return usage_exit(),
            _ => break,
        }
        i += 1;
    }

    let repo = gix::discover(".")?;
    let prefix = repo_prefix(&repo)?;
    let code = foreach_repo(&repo, &args[i..], quiet, recursive, None, prefix.as_ref())?;
    Ok(ExitCode::from(code))
}

/// One superproject's worth of `git submodule foreach`, descending in index
/// order and skipping submodules whose worktree holds no repository.
fn foreach_repo(
    repo: &gix::Repository,
    cmd: &[String],
    quiet: bool,
    recursive: bool,
    super_prefix: Option<&str>,
    prefix: Option<&BString>,
) -> Result<u8> {
    let index = repo.open_index()?;
    let entries = match module_list(repo, &index, &[])? {
        Ok(entries) => entries,
        Err(code) => return Ok(code),
    };
    let submodules = submodules(repo)?;
    let Some(workdir) = repo.workdir().map(ToOwned::to_owned) else {
        return Ok(0);
    };

    for entry in &entries {
        let Some(sub) = find_submodule(&submodules, &entry.path) else {
            continue;
        };
        let sm_dir = workdir.join(&*gix::path::from_bstr(entry.path.as_bstr()));
        let Ok(sub_repo) = gix::open(&sm_dir) else {
            continue;
        };
        let display = match super_prefix {
            Some(sp) => format!("{sp}{}", entry.path),
            None => display_path(entry.path.as_bstr(), prefix),
        };

        if !quiet {
            println!("Entering '{display}'");
            std::io::stdout().flush()?;
        }

        // An empty command list is not an error: git enters every submodule and
        // runs nothing.
        if !cmd.is_empty() {
            let status = run_in_submodule(cmd, &sm_dir, &workdir, sub.name(), entry, &display)?;
            if !status.success() {
                eprintln!("fatal: run_command returned non-zero status for {display}\n.");
                return Ok(128);
            }
        }

        if recursive {
            let nested = format!("{display}/");
            let code = foreach_repo(&sub_repo, cmd, quiet, true, Some(&nested), None)?;
            if code != 0 {
                return Ok(code);
            }
        }
    }
    Ok(0)
}

/// git runs the command through `sh`, so a single argument is a whole script and
/// several are a command with `"$@"` appended, and exports the five variables
/// `git-submodule`'s documentation promises.
fn run_in_submodule(
    cmd: &[String],
    sm_dir: &std::path::Path,
    toplevel: &std::path::Path,
    name: &BStr,
    entry: &Entry,
    display: &str,
) -> Result<std::process::ExitStatus> {
    let mut proc = std::process::Command::new("sh");
    proc.arg("-c");
    if cmd.len() == 1 {
        proc.arg(&cmd[0]);
    } else {
        proc.arg(format!("{} \"$@\"", cmd[0]));
        proc.args(&cmd[..]);
    }
    proc.current_dir(sm_dir)
        .env("name", name.to_str_lossy().as_ref())
        .env("sm_path", entry.path.to_str_lossy().as_ref())
        .env("displaypath", display)
        .env("sha1", entry.oid.to_hex().to_string())
        .env("toplevel", toplevel)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_PREFIX");
    Ok(proc.status()?)
}

// ------------------------------------------------------------------ sync ----

fn sync(args: &[String], mut quiet: bool) -> Result<ExitCode> {
    let mut patterns: Vec<BString> = Vec::new();
    let mut recursive = false;
    let mut no_more_opts = false;

    // `module_sync` parses with default `parse_options` (permutation), so a flag
    // may follow a pathspec; only `--` forces the rest to be operands.
    for a in args {
        if no_more_opts {
            patterns.push(BString::from(a.as_str()));
            continue;
        }
        match a.as_str() {
            "--" => no_more_opts = true,
            "-q" | "--quiet" => quiet = true,
            "--recursive" => recursive = true,
            s if s.starts_with('-') && s.len() > 1 => return usage_exit(),
            _ => patterns.push(BString::from(a.as_str())),
        }
    }

    if let Some(code) = reject_empty_pathspec(&patterns) {
        return Ok(code);
    }

    let repo = gix::discover(".")?;
    let prefix = repo_prefix(&repo)?;
    let code = sync_repo(&repo, &patterns, quiet, recursive, None, prefix.as_ref())?;
    Ok(ExitCode::from(code))
}

/// One superproject's worth of `git submodule sync` (`sync_submodule` per active
/// gitlink). For each active submodule it re-copies the `.gitmodules` url into
/// the superproject's `submodule.<name>.url`, and — when the submodule is
/// populated — rewrites `remote.<default-remote>.url` inside the submodule's own
/// config. `--recursive` descends with the display path carried as super-prefix.
#[allow(clippy::too_many_arguments)]
fn sync_repo(
    repo: &gix::Repository,
    patterns: &[BString],
    quiet: bool,
    recursive: bool,
    super_prefix: Option<&str>,
    prefix: Option<&BString>,
) -> Result<u8> {
    let index = repo.open_index()?;
    let entries = match module_list(repo, &index, patterns)? {
        Ok(entries) => entries,
        Err(code) => return Ok(code),
    };

    let submodules = submodules(repo)?;
    let workdir = repo.workdir().map(ToOwned::to_owned);

    // `.gitmodules` is read raw so urls are copied verbatim, exactly as git's
    // `sync_submodule` copies `sub->url` without round-tripping a parsed URL.
    let modules_path = match repo.workdir() {
        Some(wd) => wd.join(".gitmodules"),
        None => std::path::PathBuf::from(".gitmodules"),
    };
    let modules = ConfigFile::from_path_no_includes(modules_path, Source::Local).ok();

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let config_path = repo.common_dir().join("config");
    let mut config = ConfigFile::from_path_no_includes(config_path.clone(), Source::Local)?;
    let mut dirty = false;

    for entry in &entries {
        let Some(sub) = find_submodule(&submodules, &entry.path) else {
            continue;
        };
        // `sync_submodule` returns immediately for an inactive submodule.
        if !sub.is_active()? {
            continue;
        }
        let sub_name = sub.name().to_owned();
        let sub_name = sub_name.as_bstr();
        let display = match super_prefix {
            Some(sp) => format!("{sp}{}", entry.path),
            None => display_path(entry.path.as_bstr(), prefix),
        };

        // The url git copies to both the superproject and the submodule remote.
        // A relative url needs `resolve_relative_url` against the superproject's
        // default remote, which is not ported — bail rather than register a
        // literal `./`/`../` url git would have rewritten.
        let url = modules
            .as_ref()
            .and_then(|m| m.string_by("submodule", Some(sub_name), "url"));
        if let Some(u) = url.as_ref() {
            if u.starts_with(b"./") || u.starts_with(b"../") {
                bail!(
                    "submodule '{sub_name}' has the relative url {:?}; resolving it against the default remote is not ported",
                    u.to_str_lossy()
                );
            }
        }
        // git uses an empty string when the submodule has no url at all.
        let url_bytes: BString = url.unwrap_or_default();

        if !quiet {
            println!("Synchronizing submodule url for '{display}'");
            std::io::stdout().flush()?;
        }

        // Superproject `submodule.<name>.url` — git's `git_config_set_gently`.
        config.set_raw_value_by("submodule", Some(sub_name), "url", url_bytes.as_bstr())?;
        dirty = true;

        // `is_submodule_populated_gently`: no repository on disk means git stops
        // here (the remote-url rewrite and any recursion are skipped).
        let sub_repo = match workdir.as_ref() {
            Some(wd) => gix::open(wd.join(&*gix::path::from_bstr(entry.path.as_bstr()))).ok(),
            None => None,
        };
        let Some(sub_repo) = sub_repo else { continue };

        // Rewrite `remote.<default-remote>.url` in the submodule's own config.
        let remote = default_remote(&sub_repo)?;
        let remote = BString::from(remote);
        let sub_config_path = sub_repo.common_dir().join("config");
        {
            let _sub_lock = crate::lock::RepoLock::acquire(sub_repo.git_dir());
            let mut sub_config =
                ConfigFile::from_path_no_includes(sub_config_path.clone(), Source::Local)?;
            sub_config.set_raw_value_by(
                "remote",
                Some(remote.as_bstr()),
                "url",
                url_bytes.as_bstr(),
            )?;
            persist(&sub_config_path, &sub_config)?;
        }

        if recursive {
            let nested = format!("{display}/");
            let code = sync_repo(&sub_repo, &[], quiet, true, Some(&nested), None)?;
            if code != 0 {
                if dirty {
                    persist(&config_path, &config)?;
                }
                return Ok(code);
            }
        }
    }

    if dirty {
        persist(&config_path, &config)?;
    }
    Ok(0)
}

/// git's `repo_get_default_remote`: the remote of the submodule's current branch
/// (`branch.<name>.remote`), or `origin` on a detached HEAD or an unset value.
fn default_remote(repo: &gix::Repository) -> Result<String> {
    let Some(head) = repo.head_name()? else {
        // Detached HEAD.
        return Ok("origin".to_string());
    };
    let short = head.shorten().to_owned();
    let configured = repo.config_snapshot().string(KeyRef {
        section_name: "branch",
        subsection_name: Some(short.as_bstr()),
        value_name: "remote",
    });
    Ok(match configured {
        Some(v) => v.to_str_lossy().into_owned(),
        None => "origin".to_string(),
    })
}

// ---------------------------------------------------------------- update ----

/// The integration strategy a `git submodule update` may run per submodule,
/// restricted to the three that reduce to a re-exec of a ported zvcs command
/// (`checkout`/`merge`/`rebase`). git's `SM_UPDATE_NONE` is a skip and
/// `SM_UPDATE_COMMAND` (`update = !<cmd>`) runs an arbitrary shell command, so
/// neither is represented here.
#[derive(Clone, Copy, PartialEq)]
enum UpdateStrategy {
    Checkout,
    Merge,
    Rebase,
}

/// The flags of `git submodule update` this port honors. The clone/fetch-shaping
/// flags (`--depth`, `--reference`, `--recommend-shallow`, `--single-branch`,
/// `--filter`, …) still bail in `update` before any repository is touched, so they
/// never reach here.
#[derive(Clone, Copy)]
struct UpdateOpts {
    quiet: bool,
    init: bool,
    recursive: bool,
    force: bool,
    nofetch: bool,
    /// `--remote`: target the tip of the submodule's remote-tracking branch,
    /// fetched fresh, instead of the commit the superproject records (git's
    /// `update_data->remote`).
    remote: bool,
    /// The command-line update strategy (`--checkout`/`--merge`/`--rebase`), git's
    /// `update_default`; `None` means none was given and the config/.gitmodules
    /// value (else checkout) decides per submodule.
    update_default: Option<UpdateStrategy>,
}

fn update(args: &[String], quiet: bool) -> Result<ExitCode> {
    let mut opts = UpdateOpts {
        quiet,
        init: false,
        recursive: false,
        force: false,
        nofetch: false,
        remote: false,
        update_default: None,
    };
    let mut patterns: Vec<BString> = Vec::new();
    let mut no_more_opts = false;
    let mut i = 0;

    while let Some(a) = args.get(i) {
        if no_more_opts {
            patterns.push(BString::from(a.as_str()));
            i += 1;
            continue;
        }
        match a.as_str() {
            "--" => no_more_opts = true,
            "-q" | "--quiet" => opts.quiet = true,
            "-i" | "--init" => opts.init = true,
            "--recursive" => opts.recursive = true,
            "-f" | "--force" => opts.force = true,
            "-N" | "--no-fetch" => opts.nofetch = true,
            // git's `update_default` (`OPT_SET_INT`): the last of these wins.
            "--checkout" => opts.update_default = Some(UpdateStrategy::Checkout),
            "-m" | "--merge" => opts.update_default = Some(UpdateStrategy::Merge),
            "-r" | "--rebase" => opts.update_default = Some(UpdateStrategy::Rebase),
            "--remote" => opts.remote = true,
            // Clone/progress/parallelism knobs that do not change the result for
            // an already-cloned checkout: accepted and ignored.
            "--progress" => {}
            "-j" | "--jobs" => i += 1,
            s if s.starts_with("--jobs=") => {}
            // Clone-shaping options that need machinery this port does not carry —
            // the re-executed `git clone` cannot express them, so bail honestly.
            "--reference" | "--dissociate" => bail!(
                "`submodule update {a}` only affects cloning of missing submodules, which is not ported"
            ),
            s if s.starts_with("--reference=") => bail!(
                "`submodule update --reference` only affects cloning of missing submodules, which is not ported"
            ),
            "--depth" | "--recommend-shallow" | "--no-recommend-shallow" | "--single-branch"
            | "--no-single-branch" | "--require-init" => bail!(
                "`submodule update {a}` shapes the clone/fetch, which is not ported"
            ),
            s if s.starts_with("--depth=") || s.starts_with("--filter=") => bail!(
                "`submodule update {s}` shapes the clone/fetch, which is not ported"
            ),
            "--filter" => bail!(
                "`submodule update --filter` shapes the partial-clone fetch, which is not ported"
            ),
            s if s.starts_with('-') && s.len() > 1 => return usage_exit(),
            // `PARSE_OPT_STOP_AT_NON_OPTION`-style: the first operand ends option
            // parsing (git permutes here, but real invocations put flags first).
            _ => {
                patterns.push(BString::from(a.as_str()));
                no_more_opts = true;
            }
        }
        i += 1;
    }

    if let Some(code) = reject_empty_pathspec(&patterns) {
        return Ok(code);
    }

    let repo = gix::discover(".")?;
    let prefix = repo_prefix(&repo)?;
    // `warn_if_uninitialized` is set only when a pathspec was given.
    let warn = !patterns.is_empty();
    let code = update_repo(repo, &patterns, opts, None, prefix.as_ref(), warn)?;
    Ok(ExitCode::from(code))
}

/// One superproject's worth of `git submodule update`.
///
/// Mirrors `module_update` -> `update_submodules` -> `update_submodule`: an
/// optional `--init` registration pass, then per gitlink clone the submodule if
/// its worktree is empty, resolve the target commit (the recorded gitlink, or the
/// remote-tracking tip under `--remote`), fetch it in if unreachable, and check it
/// out / merge / rebase per the resolved strategy. `--recursive` descends with the
/// display path as the super-prefix, exactly as git re-invokes the helper per
/// level.
#[allow(clippy::too_many_arguments)]
fn update_repo(
    repo: gix::Repository,
    patterns: &[BString],
    opts: UpdateOpts,
    super_prefix: Option<&str>,
    prefix: Option<&BString>,
    warn: bool,
) -> Result<u8> {
    // `--init`: run the same registration pass git's `module_update` runs before
    // `update_submodules`, then re-open so the freshly-written `active`/`url`
    // config is visible to `is_active`/`update` below.
    let repo = if opts.init {
        let code = init_repo(&repo, patterns, opts.quiet)?;
        if code != 0 {
            return Ok(code);
        }
        reopen(&repo)?
    } else {
        repo
    };

    let index = repo.open_index()?;
    let entries = match module_list(&repo, &index, patterns)? {
        Ok(entries) => entries,
        Err(code) => return Ok(code),
    };
    let submodules = submodules(&repo)?;
    let Some(workdir) = repo.workdir().map(ToOwned::to_owned) else {
        return Ok(0);
    };

    for entry in &entries {
        let display = match super_prefix {
            Some(sp) => format!("{sp}{}", entry.path),
            None => display_path(entry.path.as_bstr(), prefix),
        };

        // `prepare_to_clone_next_submodule`'s skip ladder, in git's order.
        if entry.conflicted {
            eprintln!("Skipping unmerged submodule {display}");
            continue;
        }
        let Some(sub) = find_submodule(&submodules, &entry.path) else {
            warn_missing(warn, &display);
            continue;
        };

        // The `.gitmodules`/config strategy (git's `update_type`), read without the
        // command-line override so the `SM_UPDATE_NONE` skip below can fire exactly
        // when git's does: only when no `--checkout`/`--merge`/`--rebase` was given.
        let cfg_strategy = sub
            .update()?
            .unwrap_or(gix::submodule::config::Update::Checkout);
        if opts.update_default.is_none() && cfg_strategy == gix::submodule::config::Update::None {
            eprintln!("Skipping submodule '{display}'");
            continue;
        }

        if !sub.is_active()? {
            warn_missing(warn, &display);
            continue;
        }

        // git's `prepare_to_clone_next_submodule` treats a missing `<path>/.git` as
        // "needs cloning"; a just-cloned submodule then gets `suboid = null` and a
        // forced checkout of the recorded commit.
        let sm_dir = workdir.join(&*gix::path::from_bstr(entry.path.as_bstr()));
        let just_cloned = !sm_dir.join(".git").exists();
        if just_cloned {
            let sub_name = sub.name().to_owned();
            let sub_name = sub_name.as_bstr();
            // git reads `submodule.<name>.url` from config (an `init`/`update --init`
            // pass writes it there), falling back to a relative `.gitmodules` url via
            // `resolve_relative_url` — which is not ported, so a `./`/`../` url bails.
            let url = repo.config_snapshot().string(key(sub_name, "url"));
            let Some(url) = url else {
                bail!(
                    "submodule '{display}' has no registered `submodule.{sub_name}.url`; run `submodule init` (or `update --init`) first"
                );
            };
            if url.starts_with(b"./") || url.starts_with(b"../") {
                bail!(
                    "submodule '{sub_name}' has the relative url {:?}; resolving it against the default remote is not ported",
                    url.to_str_lossy()
                );
            }
            let code = clone_submodule(&url, &sm_dir, &workdir, opts.quiet)?;
            if code != 0 {
                return Ok(code);
            }
        }

        let Ok(sub_repo) = gix::open(&sm_dir) else {
            bail!("submodule path '{display}' could not be opened after cloning");
        };

        // git's `determine_submodule_update_strategy`: the command-line override,
        // else the config/.gitmodules value, else checkout; a just-cloned submodule
        // then downgrades merge/rebase (and none) to checkout.
        let strategy = match opts.update_default {
            Some(s) => s,
            None => match cfg_strategy {
                gix::submodule::config::Update::Checkout
                | gix::submodule::config::Update::None => UpdateStrategy::Checkout,
                gix::submodule::config::Update::Merge => UpdateStrategy::Merge,
                gix::submodule::config::Update::Rebase => UpdateStrategy::Rebase,
                gix::submodule::config::Update::Command(_) => bail!(
                    "submodule '{}' configures `update = !<command>`; running an arbitrary command strategy is not ported",
                    entry.path
                ),
            },
        };
        let strategy = if just_cloned {
            match strategy {
                UpdateStrategy::Merge | UpdateStrategy::Rebase => UpdateStrategy::Checkout,
                s => s,
            }
        } else {
            strategy
        };

        // `resolve_gitlink_ref(sm_path, "HEAD")`; git skips this for a just-cloned
        // submodule, treating its `suboid` as null so the procedure always runs.
        let suboid = if just_cloned {
            None
        } else {
            let Ok(head) = sub_repo.head_id() else {
                eprintln!("fatal: Unable to find current revision in submodule path '{display}'");
                return Ok(128);
            };
            Some(head.detach())
        };

        // The target: the recorded gitlink commit, or — under `--remote` — the tip
        // of the submodule's remote-tracking branch, fetched fresh.
        let oid = if opts.remote {
            match resolve_remote_oid(&repo, &sub_repo, sub, entry, &sm_dir, opts)? {
                Ok(oid) => oid,
                Err(code) => return Ok(code),
            }
        } else {
            entry.oid
        };

        // `subforce = is_null_oid(suboid) || force`; `!oideq(oid, suboid) || force`
        // otherwise the submodule is already at the target and git touches nothing.
        let subforce = suboid.is_none() || opts.force;
        if Some(oid) != suboid || opts.force {
            let code =
                run_update_procedure(&sub_repo, &sm_dir, &oid, &opts, &display, strategy, subforce)?;
            if code != 0 {
                return Ok(code);
            }
        }

        if opts.recursive {
            let nested = format!("{display}/");
            let code = update_repo(sub_repo, &[], opts, Some(&nested), None, false)?;
            if code != 0 {
                return Ok(code);
            }
        }
    }

    Ok(0)
}

/// git's `next_submodule_warn_missing`: only mentioned when paths were specified.
fn warn_missing(warn: bool, display: &str) {
    if warn {
        eprintln!("Submodule path '{display}' not initialized");
        eprintln!("Maybe you want to use 'update --init'?");
    }
}

/// git's `run_update_procedure`: fetch the target commit into the submodule if it
/// is not already reachable, then run the chosen integration strategy (checkout,
/// merge, or rebase). `subforce` is git's `is_null_oid(suboid) || force`, computed
/// by the caller (a just-cloned submodule has a null `suboid`).
fn run_update_procedure(
    sub_repo: &gix::Repository,
    sm_dir: &std::path::Path,
    oid: &ObjectId,
    opts: &UpdateOpts,
    display: &str,
    strategy: UpdateStrategy,
    subforce: bool,
) -> Result<u8> {
    if !opts.nofetch {
        // Fetch only if `oid` isn't already reachable from a ref, matching git's
        // `is_tip_reachable` guard (`rev-list -n1 <oid> --not --all`).
        if !is_tip_reachable(sub_repo, oid)? {
            let plain_failed = fetch_in_submodule(sm_dir, opts.quiet, None)? != 0;
            if plain_failed && !opts.quiet {
                eprintln!(
                    "Unable to fetch in submodule path '{display}'; trying to directly fetch {}:",
                    oid.to_hex()
                );
            }
        }
        // The usual fetch may still not have brought in `oid`; try fetching it by
        // hash directly, and fail exactly as git does if that does not help.
        if !is_tip_reachable(sub_repo, oid)? {
            let remote = default_remote(sub_repo)?;
            if fetch_in_submodule(sm_dir, opts.quiet, Some((remote.as_str(), oid)))? != 0 {
                eprintln!(
                    "fatal: Fetched in submodule path '{display}', but it did not contain {}. Direct fetching of that commit failed.",
                    oid.to_hex()
                );
                return Ok(128);
            }
        }
    }

    run_update_command(sm_dir, strategy, oid, subforce, opts.quiet, display)
}

/// git's `is_tip_reachable`: whether `oid` is already reachable from one of the
/// submodule's refs — the object exists and `rev-list <oid> --not --all` is
/// empty. Implemented in-process as a hidden-tip revision walk from `oid`.
fn is_tip_reachable(repo: &gix::Repository, oid: &ObjectId) -> Result<bool> {
    // Ref tips to hide (git's `--all`), keeping only those that peel to a commit.
    let mut tips: Vec<ObjectId> = Vec::new();
    let refs = repo.references()?;
    for r in refs.all()? {
        let Ok(mut r) = r else { continue };
        if let Ok(id) = r.peel_to_id() {
            let id = id.detach();
            if repo
                .find_object(id)
                .ok()
                .and_then(|o| o.peel_to_commit().ok())
                .is_some()
            {
                tips.push(id);
            }
        }
    }

    // A walk that fails to start means `oid` is absent (or not a commit): git's
    // `rev-list` would error too, so the tip is not reachable.
    let mut walk = match repo.rev_walk(Some(*oid)).with_hidden(tips).all() {
        Ok(walk) => walk,
        Err(_) => return Ok(false),
    };
    // Any item emitted (a commit reachable from `oid` but not from a ref, or a
    // traversal error on a missing object) means `oid` is not covered by the
    // refs, exactly as a non-empty / failing `rev-list` does; empty means it is.
    Ok(walk.next().is_none())
}

/// git's `fetch_in_submodule`: `git fetch [--quiet] [<remote> <hash>]` run inside
/// the submodule. Re-executes this binary — the faithful analogue of git's
/// `cp.git_cmd = 1; cp.dir = module_path` child — so the fetch rides the vendored
/// gix blocking transport. Returns the child's exit code (0 on success).
fn fetch_in_submodule(
    sm_dir: &std::path::Path,
    quiet: bool,
    direct: Option<(&str, &ObjectId)>,
) -> Result<u8> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("fetch");
    if quiet {
        cmd.arg("--quiet");
    }
    if let Some((remote, oid)) = direct {
        cmd.arg(remote).arg(oid.to_hex().to_string());
    }
    submodule_child_env(&mut cmd, sm_dir);
    let status = cmd.status()?;
    Ok(child_code(status))
}

/// git's `run_update_command`: re-exec the ported subcommand for the chosen
/// strategy inside the submodule — `git checkout -q [-f] <oid>`, `git merge
/// [--quiet] <oid>`, or `git rebase [--quiet] <oid>` — then print git's success
/// line. On failure git prints the strategy's fatal; checkout returns the child's
/// own exit code, while merge/rebase return `die_message`'s 128.
fn run_update_command(
    sm_dir: &std::path::Path,
    strategy: UpdateStrategy,
    oid: &ObjectId,
    subforce: bool,
    quiet: bool,
    display: &str,
) -> Result<u8> {
    let hex = oid.to_hex().to_string();
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    match strategy {
        UpdateStrategy::Checkout => {
            cmd.arg("checkout").arg("-q");
            if subforce {
                cmd.arg("-f");
            }
        }
        UpdateStrategy::Merge => {
            cmd.arg("merge");
            if quiet {
                cmd.arg("--quiet");
            }
        }
        UpdateStrategy::Rebase => {
            cmd.arg("rebase");
            if quiet {
                cmd.arg("--quiet");
            }
        }
    }
    cmd.arg(&hex);
    submodule_child_env(&mut cmd, sm_dir);
    let status = cmd.status()?;
    let code = child_code(status);
    if code != 0 {
        // git's checkout branch keeps `git checkout`'s exit code; merge/rebase
        // replace it with `die_message`'s 128.
        return Ok(match strategy {
            UpdateStrategy::Checkout => {
                eprintln!("fatal: Unable to checkout '{hex}' in submodule path '{display}'");
                code
            }
            UpdateStrategy::Merge => {
                eprintln!("fatal: Unable to merge '{hex}' in submodule path '{display}'");
                128
            }
            UpdateStrategy::Rebase => {
                eprintln!("fatal: Unable to rebase '{hex}' in submodule path '{display}'");
                128
            }
        });
    }
    if !quiet {
        match strategy {
            UpdateStrategy::Checkout => {
                println!("Submodule path '{display}': checked out '{hex}'")
            }
            UpdateStrategy::Merge => println!("Submodule path '{display}': merged in '{hex}'"),
            UpdateStrategy::Rebase => println!("Submodule path '{display}': rebased into '{hex}'"),
        }
        std::io::stdout().flush()?;
    }
    Ok(0)
}

/// git's `clone_submodule`, approximated by re-executing the ported `git clone
/// [--quiet] <url> <path>` from the superproject worktree with a clean repository
/// env. Returns the child's exit code (0 on success). This yields an embedded
/// `<path>/.git` rather than git's separate `.git/modules/<name>` gitdir, and
/// carries none of the clone-shaping flags (`--depth`/`--reference`/…) — those
/// stay floored in `update`'s parser.
fn clone_submodule(
    url: &BString,
    sm_dir: &std::path::Path,
    toplevel: &std::path::Path,
    quiet: bool,
) -> Result<u8> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("clone");
    if quiet {
        cmd.arg("--quiet");
    }
    cmd.arg(url.to_str_lossy().as_ref()).arg(sm_dir);
    cmd.current_dir(toplevel)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_PREFIX");
    let status = cmd.status()?;
    Ok(child_code(status))
}

/// git's `update_submodule` `--remote` block: resolve the tip of the submodule's
/// remote-tracking branch. Determines the default remote (git's
/// `get_default_remote_submodule`) and branch (git's `remote_submodule_branch`),
/// fetches unless `--no-fetch`, then reads `refs/remotes/<remote>/<branch>`.
/// `Err(code)` carries the exit code of a `die_message` failure.
fn resolve_remote_oid(
    super_repo: &gix::Repository,
    sub_repo: &gix::Repository,
    sub: &gix::Submodule,
    entry: &Entry,
    sm_dir: &std::path::Path,
    opts: UpdateOpts,
) -> Result<std::result::Result<ObjectId, u8>> {
    let remote_name = default_remote(sub_repo)?;
    let branch = match remote_submodule_branch(super_repo, sub)? {
        Ok(b) => b,
        Err(code) => return Ok(Err(code)),
    };
    let remote_ref = format!("refs/remotes/{remote_name}/{branch}");

    // git fetches with `quiet = 0` here regardless of `--quiet`, and reports the
    // failure against the raw submodule path (`sm_path`), not the display path.
    if !opts.nofetch && fetch_in_submodule(sm_dir, false, None)? != 0 {
        eprintln!(
            "fatal: Unable to fetch in submodule path '{}'",
            entry.path
        );
        return Ok(Err(128));
    }

    // `resolve_gitlink_ref(sm_path, remote_ref)`: any lookup/peel failure dies.
    match sub_repo.try_find_reference(remote_ref.as_str()) {
        Ok(Some(mut r)) => match r.peel_to_id() {
            Ok(id) => Ok(Ok(id.detach())),
            Err(_) => {
                eprintln!(
                    "fatal: Unable to find {remote_ref} revision in submodule path '{}'",
                    entry.path
                );
                Ok(Err(128))
            }
        },
        _ => {
            eprintln!(
                "fatal: Unable to find {remote_ref} revision in submodule path '{}'",
                entry.path
            );
            Ok(Err(128))
        }
    }
}

/// git's `remote_submodule_branch`: the tracking branch of a `--remote` update.
/// `submodule.<name>.branch` (config over `.gitmodules`) when set, else `HEAD`;
/// a `.` value inherits the superproject's current branch, dying when it is
/// detached. `Err(code)` carries the `die_message` exit code.
fn remote_submodule_branch(
    super_repo: &gix::Repository,
    sub: &gix::Submodule,
) -> Result<std::result::Result<String, u8>> {
    let name = match sub.branch()? {
        None => "HEAD".to_string(),
        Some(gix::submodule::config::Branch::Name(b)) => b.to_str_lossy().into_owned(),
        Some(gix::submodule::config::Branch::CurrentInSuperproject) => match super_repo.head_name()?
        {
            Some(full) => full.shorten().to_str_lossy().into_owned(),
            None => {
                eprintln!(
                    "fatal: Submodule ({}) branch configured to inherit branch from superproject, but the superproject is not on any branch",
                    sub.name()
                );
                return Ok(Err(128));
            }
        },
    };
    Ok(Ok(name))
}

/// Point a re-executed child at the submodule worktree and clear the inherited
/// repository env, git's `prepare_submodule_repo_env` for a `git_cmd` child.
fn submodule_child_env(cmd: &mut std::process::Command, sm_dir: &std::path::Path) {
    cmd.current_dir(sm_dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_PREFIX");
}

/// A child process's exit code, mapping a signal death to git's `128 + signal`.
fn child_code(status: std::process::ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return code as u8;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return (128 + signal) as u8;
        }
    }
    128
}

/// Re-open a repository so a config change written to disk (e.g. by the `--init`
/// registration pass) is reflected in the in-memory snapshot the update loop
/// reads. gix caches config at open time, so the write is otherwise invisible.
fn reopen(repo: &gix::Repository) -> Result<gix::Repository> {
    let path = repo
        .workdir()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| repo.git_dir().to_owned());
    Ok(gix::open(path)?)
}

// ------------------------------------------------------------ set-branch ----

/// The `usage:` block stock git prints for a `set-branch` operand-count error,
/// verbatim from `git submodule--helper`'s `parse_options` (exit 129). Captured
/// byte-for-byte from git 2.55.0 (`git submodule set-branch -b x a b`); the help
/// column is padded to 26 and the block ends with a trailing blank line.
const SET_BRANCH_USAGE: &str = "\
usage: git submodule set-branch [-q|--quiet] (-d|--default) <path>
   or: git submodule set-branch [-q|--quiet] (-b|--branch) <branch> <path>

    -d, --[no-]default    set the default tracking branch to master
    -b, --[no-]branch <branch>
                          set the default tracking branch

";

/// Print the set-branch usage block and hand back git's exit code (129) for a
/// wrong operand count — distinct from the top-level `usage_exit` (1) that the
/// porcelain wrapper raises for an unrecognized option.
fn set_branch_usage_exit() -> Result<ExitCode> {
    eprint!("{SET_BRANCH_USAGE}");
    Ok(ExitCode::from(129))
}

/// The outcome of parsing a `set-branch` command line, before any repository is
/// touched. Splitting the pure parse out keeps git's exact exit-code ladder
/// (top-usage 1, subcommand-usage 129, `die` 128) unit-testable.
#[derive(Debug, PartialEq)]
enum SetBranch {
    /// An unrecognized/malformed option: the porcelain wrapper's top-level usage
    /// (exit 1). Covers `-b`/`--branch` with a missing or empty value.
    UsageTop,
    /// A wrong operand count: the set-branch subcommand usage (exit 129).
    UsageSub,
    /// Neither `--branch` nor `--default`: `die` with 128.
    Required,
    /// Both `--branch` and `--default`: `die` with 128.
    Both,
    /// A well-formed request: set `branch` to `Some(value)`, or remove it (i.e.
    /// `--default`) when `branch` is `None`.
    Apply { branch: Option<String>, path: String },
}

/// Mirror of `git-submodule.sh`'s `cmd_set_branch` porcelain parsing followed by
/// `module_set_branch`'s validation ladder. The porcelain does exact-match option
/// parsing (no bundling, no abbreviation) and stops at the first operand, forwards
/// only `--branch`/`--default` plus the operands, and the helper then enforces the
/// required/both/operand-count rules in that order.
fn classify_set_branch(args: &[String]) -> SetBranch {
    let mut default = false;
    let mut branch: Option<String> = None;
    let mut operands: Vec<String> = Vec::new();
    let mut end_opts = false;
    let mut i = 0;

    while let Some(a) = args.get(i) {
        if end_opts {
            operands.push(a.clone());
            i += 1;
            continue;
        }
        match a.as_str() {
            // Accepted for uniformity; there is nothing to quiet in set-branch.
            "-q" | "--quiet" => {}
            "-d" | "--default" => default = true,
            // `-b`/`--branch` takes the next token as the value; a missing or
            // empty value is the porcelain's `case "$2" in '') usage` (exit 1).
            "-b" | "--branch" => match args.get(i + 1) {
                Some(v) if !v.is_empty() => {
                    branch = Some(v.clone());
                    i += 1;
                }
                _ => return SetBranch::UsageTop,
            },
            // The `--branch=<v>` form is accepted verbatim, empty value included.
            s if s.starts_with("--branch=") => {
                branch = Some(s["--branch=".len()..].to_string());
            }
            "--" => end_opts = true,
            // Any other dash-prefixed token (`-*`) is rejected by the wrapper.
            s if s.starts_with('-') => return SetBranch::UsageTop,
            // The first operand ends option parsing; the rest are operands too.
            _ => {
                operands.push(a.clone());
                end_opts = true;
            }
        }
        i += 1;
    }

    if branch.is_none() && !default {
        return SetBranch::Required;
    }
    if branch.is_some() && default {
        return SetBranch::Both;
    }
    if operands.len() != 1 {
        return SetBranch::UsageSub;
    }
    SetBranch::Apply {
        branch,
        path: operands.pop().expect("checked len == 1"),
    }
}

/// `git submodule set-branch` — record (or clear) a submodule's default tracking
/// branch in `.gitmodules`. `_quiet` is consumed for parity; set-branch prints
/// nothing on success either way.
fn set_branch(args: &[String], _quiet: bool) -> Result<ExitCode> {
    match classify_set_branch(args) {
        SetBranch::UsageTop => usage_exit(),
        SetBranch::UsageSub => set_branch_usage_exit(),
        SetBranch::Required => {
            eprintln!("fatal: --branch or --default required");
            Ok(ExitCode::from(128))
        }
        SetBranch::Both => {
            eprintln!("fatal: options '--branch' and '--default' cannot be used together");
            Ok(ExitCode::from(128))
        }
        SetBranch::Apply { branch, path } => set_branch_apply(branch, path),
    }
}

/// Resolve `<path>` to a submodule name via `.gitmodules`, then set or remove
/// `submodule.<name>.branch`. git's `config_set_in_gitmodules_file_gently`
/// returns `!!ret`, so a `--default` that removes nothing exits 1 with no output.
fn set_branch_apply(branch: Option<String>, path: String) -> Result<ExitCode> {
    let path = BString::from(path.as_str());
    let repo = gix::discover(".")?;
    let submodules = submodules(&repo)?;
    let prefix = repo_prefix(&repo)?;

    // git looks the path up relative to the repository root, so a run from a
    // subdirectory carries the cwd prefix into the `.gitmodules` `path` match.
    let full = match prefix.as_ref() {
        Some(p) => {
            let mut b = p.clone();
            b.extend_from_slice(&path);
            b
        }
        None => path,
    };

    let Some(sub) = find_submodule(&submodules, &full) else {
        eprintln!("fatal: no submodule mapping found in .gitmodules for path '{full}'");
        return Ok(ExitCode::from(128));
    };
    let sub_name = sub.name().to_owned();
    let sub_name = sub_name.as_bstr();

    let modules_path = match repo.workdir() {
        Some(wd) => wd.join(".gitmodules"),
        None => std::path::PathBuf::from(".gitmodules"),
    };
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let mut modules = ConfigFile::from_path_no_includes(modules_path.clone(), Source::Local)?;

    match branch {
        // `set-branch --branch <b>`: write the key, keyed by submodule name.
        Some(value) => {
            modules.set_raw_value_by("submodule", Some(sub_name), "branch", value.as_str())?;
            persist(&modules_path, &modules)?;
            Ok(ExitCode::SUCCESS)
        }
        // `set-branch --default`: drop the key; exit 1 when there was none to drop.
        None => {
            let removed = modules
                .section_mut("submodule", Some(sub_name))
                .ok()
                .and_then(|mut s| s.remove("branch"))
                .is_some();
            if removed {
                persist(&modules_path, &modules)?;
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(1))
            }
        }
    }
}

/// A `submodule.<name>.<field>` config key, built structurally so submodule
/// names containing dots still resolve to the right subsection.
fn key<'a>(name: &'a BStr, field: &'a str) -> KeyRef<'a> {
    KeyRef {
        section_name: "submodule",
        subsection_name: Some(name),
        value_name: field,
    }
}

/// Serialize `file` next to `path` and rename it into place, so a crash never
/// leaves a half-written config. Mirrors `porcelain::config`'s writer.
fn persist(path: &std::path::Path, file: &ConfigFile) -> Result<()> {
    let bytes = file.to_bstring();
    let tmp = path.with_extension("zvcs-tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ---------------------------------------------------------- .gitmodules -----

/// Every submodule declared in `.gitmodules`, or an empty list when the file is
/// absent (git treats that as "no mappings", then dies per gitlink entry).
fn submodules(repo: &gix::Repository) -> Result<Vec<gix::Submodule<'_>>> {
    Ok(match repo.submodules()? {
        Some(iter) => iter.collect(),
        None => Vec::new(),
    })
}

/// The declaration whose `path` field names `path`, if any.
fn find_submodule<'a, 'repo>(
    submodules: &'a [gix::Submodule<'repo>],
    path: &BString,
) -> Option<&'a gix::Submodule<'repo>> {
    submodules
        .iter()
        .find(|s| s.path().map(|p| &p == path).unwrap_or(false))
}

// ----------------------------------------------------------- module list ----

/// git's `module_list_compute`: index entries with a gitlink mode, selected by
/// `patterns`, one row per path even when several merge stages are present.
///
/// `Err(code)` carries git's exit code for pathspecs that matched nothing.
fn module_list(
    repo: &gix::Repository,
    index: &gix::index::State,
    patterns: &[BString],
) -> Result<std::result::Result<Vec<Entry>, u8>> {
    // Unmatched pathspecs are reported before anything is listed, and are an
    // error even when a *different* pathspec did match.
    let mut unmatched = false;
    for pattern in patterns {
        if !pathspec_matches_any(repo, index, std::slice::from_ref(pattern))? {
            eprintln!("error: pathspec '{pattern}' did not match any file(s) known to git");
            unmatched = true;
        }
    }
    if unmatched {
        return Ok(Err(1));
    }

    // `empty_patterns_match_prefix = false`: a bare `git submodule status` run
    // from a subdirectory still lists every submodule, not just those below it.
    let mut ps = repo.pathspec(
        false,
        patterns,
        false,
        index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;

    let mut entries: Vec<Entry> = Vec::new();
    if let Some(iter) = ps.index_entries_with_paths(index) {
        for (path, entry) in iter {
            if entry.mode != gix::index::entry::Mode::COMMIT {
                continue;
            }
            // Index entries are path-sorted, so duplicate stages are adjacent.
            let dup = entries
                .last()
                .map(|e| e.path.as_bstr() == path)
                .unwrap_or(false);
            if dup {
                continue;
            }
            entries.push(Entry {
                path: path.to_owned(),
                oid: entry.id,
                conflicted: entry.stage_raw() != 0,
            });
        }
    }
    Ok(Ok(entries))
}

/// Whether `patterns` select at least one index entry — git's `ps_matched`
/// bookkeeping, evaluated one pathspec at a time.
fn pathspec_matches_any(
    repo: &gix::Repository,
    index: &gix::index::State,
    patterns: &[BString],
) -> Result<bool> {
    let mut ps = repo.pathspec(
        false,
        patterns,
        false,
        index,
        gix::worktree::stack::state::attributes::Source::IdMapping,
    )?;
    // Bound before returning: the iterator borrows `ps`, so the answer must be
    // reduced to a bool while `ps` is still alive.
    let matched = match ps.index_entries_with_paths(index) {
        Some(mut iter) => iter.next().is_some(),
        None => false,
    };
    Ok(matched)
}

// -------------------------------------------------------------- rev name ----

/// git's `compute_rev_name`: the first of four `git describe` invocations that
/// succeeds, or `None` when all of them fail (which includes the case where
/// `oid` is not present in the submodule's object database at all).
fn rev_name(repo: &gix::Repository, oid: &ObjectId) -> Result<Option<String>> {
    let commit = match repo.find_object(*oid) {
        Ok(obj) => match obj.peel_to_commit() {
            Ok(commit) => commit,
            Err(_) => return Ok(None),
        },
        Err(_) => return Ok(None),
    };

    // 1. `git describe` — annotated tags only. 2. `git describe --tags`.
    for select in [SelectRef::AnnotatedTags, SelectRef::AllTags] {
        let platform = commit.describe().names(select);
        if let Some(resolution) = platform.try_resolve()? {
            return Ok(Some(resolution.format()?.to_string()));
        }
    }

    // 3. `git describe --contains` is `git name-rev`, a different algorithm that
    // the vendored crates do not implement. It can only produce a name when the
    // submodule holds tags, so falling through is safe exactly when it has none.
    let refs = repo.references()?;
    if refs.tags()?.next().is_some() {
        bail!(
            "naming {oid} needs `git describe --contains` (name-rev), which is not ported; \
             the submodule has tags that neither `describe` nor `describe --tags` reached"
        );
    }
    drop(refs);

    // 4. `git describe --all --always`.
    describe_all_always(repo, oid)
}

/// `git describe --all --always <oid>`, with the candidate table built the way
/// git's `get_name()` does under `--all`: keyed by the peeled object id, named
/// by the full ref name minus `refs/`, and won by the highest priority
/// (annotated tag > lightweight tag > any other ref), ties going to the newest
/// tagger date and then to the first ref in refname order.
fn describe_all_always(repo: &gix::Repository, oid: &ObjectId) -> Result<Option<String>> {
    // (full ref name, peeled id, priority, tagger date)
    let mut candidates: Vec<(BString, ObjectId, u8, i64)> = Vec::new();
    {
        let refs = repo.references()?;
        for r in refs.all()? {
            let Ok(mut r) = r else { continue };
            let full = r.name().as_bstr().to_owned();
            if !full.starts_with(b"refs/") {
                continue;
            }
            let target = r.target().try_id().map(ToOwned::to_owned);
            let Ok(peeled) = r.peel_to_id() else { continue };
            let peeled = peeled.detach();

            let is_tag = full.starts_with(b"refs/tags/");
            let (annotated, tag_date) = match target {
                // A ref whose direct target differs from its peeled id is an
                // annotated tag; its tagger date breaks ties between two of them.
                Some(target) if target != peeled => match repo
                    .find_object(target)
                    .ok()
                    .and_then(|o| o.try_into_tag().ok())
                {
                    Some(tag) => (
                        true,
                        tag.tagger()
                            .ok()
                            .and_then(|s| s.map(|s| s.seconds()))
                            .unwrap_or(0),
                    ),
                    None => (false, 0),
                },
                _ => (false, 0),
            };
            let prio = if annotated {
                2
            } else if is_tag {
                1
            } else {
                0
            };
            candidates.push((full, peeled, prio, tag_date));
        }
    }
    // git iterates refs in refname order; the fold below keeps the first winner.
    candidates.sort_by(|a, b| a.0.cmp(&b.0));

    let mut best: HashMap<ObjectId, (u8, i64)> = HashMap::new();
    let mut options = gix::revision::plumbing::describe::Options {
        name_by_oid: Default::default(),
        max_candidates: 10,
        fallback_to_oid: true,
        first_parent: false,
    };
    for (full, peeled, prio, tag_date) in candidates {
        let replace = match best.get(&peeled) {
            None => true,
            Some(&(have_prio, have_date)) => {
                have_prio < prio || (have_prio == 2 && prio == 2 && have_date < tag_date)
            }
        };
        if !replace {
            continue;
        }
        best.insert(peeled, (prio, tag_date));
        // `refs/heads/main` → `heads/main`, `refs/tags/v1` → `tags/v1`.
        let name = BString::from(&full["refs/".len()..]);
        options.name_by_oid.insert(peeled, Cow::Owned(name));
    }

    let cache = repo.commit_graph_if_enabled()?;
    let mut graph = repo.revision_graph(cache.as_ref());
    let outcome = gix::revision::plumbing::describe(oid, &mut graph, options)
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    drop(graph);

    let Some(outcome) = outcome else {
        return Ok(None);
    };
    let hex_len = (*oid).attach(repo).shorten()?.hex_len();
    Ok(Some(outcome.into_format(hex_len).to_string()))
}

// ------------------------------------------------------------ path display --

/// The repository-to-cwd prefix, with a trailing `/`, or `None` at the top level.
fn repo_prefix(repo: &gix::Repository) -> Result<Option<BString>> {
    Ok(match repo.prefix()? {
        Some(p) if !p.as_os_str().is_empty() => {
            let mut b = gix::path::into_bstr(p).into_owned();
            b.push(b'/');
            Some(b)
        }
        _ => None,
    })
}

/// git's `get_submodule_displaypath`: the repository-root-relative `path`
/// re-expressed relative to `prefix` (itself root-relative, with a trailing `/`).
fn display_path(path: &BStr, prefix: Option<&BString>) -> String {
    let path = path.to_str_lossy();
    let Some(prefix) = prefix else {
        return path.into_owned();
    };
    let prefix = prefix.to_str_lossy();

    let from: Vec<&str> = prefix.split('/').filter(|s| !s.is_empty()).collect();
    let to: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let mut out = String::new();
    for _ in common..from.len() {
        out.push_str("../");
    }
    out.push_str(&to[common..].join("/"));
    if out.is_empty() {
        "./".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cls(args: &[&str]) -> SetBranch {
        classify_set_branch(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    fn apply(branch: Option<&str>, path: &str) -> SetBranch {
        SetBranch::Apply {
            branch: branch.map(str::to_string),
            path: path.to_string(),
        }
    }

    /// Every case below is the observed behavior of stock `git submodule
    /// set-branch` (git 2.55.0); the enum variant maps 1:1 to git's exit code
    /// (UsageTop=1, UsageSub=129, Required/Both=128, Apply=0/1).
    #[test]
    fn set_branch_parse_matches_git() {
        // Well-formed writes.
        assert_eq!(cls(&["-b", "feature", "sub/foo"]), apply(Some("feature"), "sub/foo"));
        assert_eq!(cls(&["--branch", "feat", "sub/foo"]), apply(Some("feat"), "sub/foo"));
        assert_eq!(cls(&["--branch=feat", "sub/foo"]), apply(Some("feat"), "sub/foo"));
        assert_eq!(cls(&["--branch=", "sub/foo"]), apply(Some(""), "sub/foo"));
        assert_eq!(cls(&["-q", "-b", "feature", "sub/foo"]), apply(Some("feature"), "sub/foo"));
        assert_eq!(cls(&["-b", "feature", "--", "sub/foo"]), apply(Some("feature"), "sub/foo"));
        // `--default` removes the key (branch == None).
        assert_eq!(cls(&["-d", "sub/foo"]), apply(None, "sub/foo"));
        assert_eq!(cls(&["--default", "sub/foo"]), apply(None, "sub/foo"));

        // Neither flag -> `--branch or --default required` (128). A leading
        // operand stops option parsing, so trailing `-b feature` is an operand.
        assert_eq!(cls(&["sub/foo"]), SetBranch::Required);
        assert_eq!(cls(&["sub/foo", "-b", "feature"]), SetBranch::Required);

        // Both flags -> cannot be used together (128).
        assert_eq!(cls(&["-b", "x", "-d", "sub/foo"]), SetBranch::Both);

        // Wrong operand count -> subcommand usage (129). `--branch sub/foo`
        // consumes the path as the value, leaving zero operands.
        assert_eq!(cls(&["-b", "feature", "sub/foo", "extra"]), SetBranch::UsageSub);
        assert_eq!(cls(&["--branch", "sub/foo"]), SetBranch::UsageSub);
        assert_eq!(cls(&["-d"]), SetBranch::UsageSub);

        // Malformed/unknown option -> top-level usage (exit 1).
        assert_eq!(cls(&["-b", ""]), SetBranch::UsageTop);
        assert_eq!(cls(&["-b", "", "sub/foo"]), SetBranch::UsageTop);
        assert_eq!(cls(&["--branch"]), SetBranch::UsageTop);
        assert_eq!(cls(&["--bogus", "sub/foo"]), SetBranch::UsageTop);
        assert_eq!(cls(&["-db", "sub/foo"]), SetBranch::UsageTop);
        assert_eq!(cls(&["--def", "sub/foo"]), SetBranch::UsageTop);
    }

    /// Pins the 129 usage block to the exact bytes git emits (see the const's
    /// provenance note): the two `usage:`/`or:` lines and the trailing blank line.
    #[test]
    fn set_branch_usage_bytes_match_git() {
        assert!(SET_BRANCH_USAGE.starts_with(
            "usage: git submodule set-branch [-q|--quiet] (-d|--default) <path>\n   or: git submodule set-branch [-q|--quiet] (-b|--branch) <branch> <path>\n\n"
        ));
        assert!(SET_BRANCH_USAGE.ends_with("set the default tracking branch\n\n"));
    }
}
