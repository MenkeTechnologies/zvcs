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
///   * `git submodule [--quiet] set-url [--] <path> <newurl>`
///     Writes `submodule.<name>.url` into the worktree `.gitmodules`, then runs
///     the same `sync_submodule` pass `sync` runs for that one submodule, so the
///     new url reaches `.git/config` and the submodule's own remote and the
///     `Synchronizing submodule url for '<displaypath>'` line is printed. A
///     wrong operand count prints the set-url usage and exits 129.
///
///   * `git submodule [--quiet] deinit [-f|--force] (--all | [--] <path>...)`
///     Empties each listed submodule's worktree directory and removes the whole
///     `submodule.<name>` section from `.git/config`, leaving `.gitmodules` and
///     the gitlink alone. Without `-f` a `git rm -qn` dry run first refuses a
///     worktree carrying local modifications; a `.git` *directory* inside the
///     worktree is absorbed into the superproject before the removal, and the
///     submodule's own `core.worktree` is unset after it. Prints
///     `Cleared directory '<displaypath>'` and `Submodule '<name>' (<url>)
///     unregistered for path '<displaypath>'`. Neither `--all` nor a path dies
///     128; both together print the deinit usage and exit 129.
///
///   * `git submodule absorbgitdirs [--] [<path>...]`
///     Moves any submodule whose repository still lives in its own worktree into
///     the superproject's `modules/<name>`, leaving a `gitdir:` file and a
///     matching `core.worktree` behind, then recurses. A submodule that is
///     already absorbed, or not populated at all, is left untouched — which is
///     why the common case prints nothing and exits 0.
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
/// `-h` as the very first argument is `git-sh-setup`'s, not the script's: the
/// usage block goes to **stdout** and the exit status is 0, unlike every parse
/// error, which prints the same block on stderr and exits 1.
///
/// All eleven subcommands are ported. What still bails, in every one of them, is
/// a `.gitmodules` url that is relative (`./`, `../`) — git's
/// `resolve_relative_url` resolves it against the superproject's default remote,
/// and that is not ported — plus, in `update`, the clone/fetch-shaping flags
/// (`--depth`, `--reference`, `--dissociate`, `--recommend-shallow`,
/// `--single-branch`, `--filter`, `--require-init`) and a `!command` update
/// strategy.
///
/// Every url this porcelain dials comes out of `.gitmodules` rather than off the
/// command line, so — exactly as `git-submodule.sh:29` does — `GIT_PROTOCOL_FROM_USER`
/// is cleared on entry and the `protocol.<type>.allow` policy then refuses a
/// `file` url unless it was explicitly permitted. `git submodule--helper` does
/// *not* do this, which is why it reaches [`subcommand`] directly.
pub fn submodule(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands us the subcommand at index 0; tolerate both conventions so
    // the wiring may pass either `["submodule", ...]` or just the tail.
    let args = match args.first() {
        Some(a) if a == "submodule" => &args[1..],
        _ => args,
    };

    // `-h` is handled by `git-sh-setup` before `git-submodule.sh` parses
    // anything of its own: `case "$1" in -h) echo "$LONG_USAGE"; exit`, i.e. the
    // usage block on **stdout** and a bare `exit`, which is status 0. Only an
    // exact first argument counts — `git submodule --quiet -h` and
    // `git submodule status -h` never reach it — and trailing arguments are
    // ignored, since the `case` never looks at `$#`.
    if args.first().map(String::as_str) == Some("-h") {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }

    // `git-submodule.sh:29`: "Tell the rest of git that any URLs we get don't
    // come directly from the user, so it can apply policy as appropriate." Every
    // url this porcelain dials comes out of `.gitmodules`, so `protocol.<x>.allow
    // = user` (the default for `file`, CVE-2022-39253) must refuse it. The
    // assignment is unconditional in the script, so an inherited
    // `GIT_PROTOCOL_FROM_USER=1` is overwritten — and it is set here rather than
    // in `submodule--helper`, which is why `git submodule--helper update
    // --remote` still fetches over `file` where `git submodule update --remote`
    // does not.
    std::env::set_var("GIT_PROTOCOL_FROM_USER", "0");

    subcommand(args)
}

/// The subcommand table `git submodule` and `git submodule--helper` share.
///
/// Every name below is registered in builtin/submodule--helper.c's
/// `OPT_SUBCOMMAND` table against the very same C function the porcelain's
/// `cmd_<name>` shell wrapper dispatches to, so the helper reaches the
/// implementation here rather than re-entering [`submodule`] — which matters
/// because [`submodule`] also reproduces `git-submodule.sh`'s
/// `GIT_PROTOCOL_FROM_USER=0` export, and the helper deliberately does not have
/// it.
pub(super) fn subcommand(args: &[String]) -> Result<ExitCode> {
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
        "add" => add(tail, quiet),
        "update" => update(tail, quiet),
        "deinit" => deinit(tail, quiet),
        "sync" => sync(tail, quiet),
        "set-branch" => set_branch(tail, quiet),
        "set-url" => set_url(tail, quiet),
        "absorbgitdirs" => absorbgitdirs(tail),
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
        let active = is_submodule_active(repo, &index, sub, &entry.path)?;
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
                Some(sub) => is_submodule_active(repo, &index, sub, &entry.path)?,
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
        if !is_submodule_active(repo, &index, sub, &entry.path)? {
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
                    _ => crate::git_fatal!("submodule '{sub_name}' has an unknown update strategy {upd:?}"),
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
            lines.sort_by_key(|x| std::cmp::Reverse(x.0));
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
    // git's `toplevel` is `xgetcwd()`, read after `git-submodule.sh` ran
    // `cd_to_toplevel` (and after git's own setup chdir'd to the top level), so
    // it is the *resolved absolute* worktree path — never the `.` that
    // `Repository::workdir` hands back for a repository opened in place.
    // Recursion re-enters this function on the submodule's own repository, whose
    // worktree is the submodule directory, which is what git's `cp.dir = path`
    // child sees as its cwd.
    let toplevel = workdir.canonicalize().unwrap_or_else(|_| absolute(&workdir));

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
            let status = run_in_submodule(cmd, &sm_dir, &toplevel, sub.name(), entry, &display)?;
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

/// git's `runcommand_in_submodule_cb`, minus the `Entering` line and the
/// recursion the caller owns.
///
/// The one-argument and many-argument forms are deliberately *not* equivalent,
/// and git says so in a `NEEDSWORK` comment: only a single argument gets the
/// five exported variables and the `path=<sq-quoted>; ` prologue, "for
/// maintaining a faithful translation from shell script". Several arguments are
/// handed to `run_command` as a plain argv with `use_shell = 1`.
fn run_in_submodule(
    cmd: &[String],
    sm_dir: &std::path::Path,
    toplevel: &std::path::Path,
    name: &BStr,
    entry: &Entry,
    display: &str,
) -> Result<std::process::ExitStatus> {
    let mut proc = if cmd.len() == 1 {
        // `strvec_pushf(&cp.args, "path=%s; %s", sq_quote(path), argv[0])`. The
        // assignment always carries a `;` and a space, so `prepare_shell_cmd`'s
        // metacharacter test always fires and the script always runs under `sh`.
        let mut proc = std::process::Command::new("sh");
        proc.arg("-c")
            .arg(format!("path={}; {}", sq_quote(entry.path.as_bstr()), cmd[0]))
            .env("name", name.to_str_lossy().as_ref())
            .env("sm_path", entry.path.to_str_lossy().as_ref())
            .env("displaypath", display)
            .env("sha1", entry.oid.to_hex().to_string())
            .env("toplevel", toplevel);
        proc
    } else {
        shell_command(cmd)
    };
    // git's `prepare_submodule_repo_env` for the child.
    proc.current_dir(sm_dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_PREFIX");
    Ok(proc.status()?)
}

/// git's `prepare_shell_cmd` (run-command.c) for a `use_shell = 1` child: a
/// first word free of shell metacharacters is executed directly, otherwise the
/// argv becomes `sh -c '<argv0> "$@"' <argv...>` (or just `sh -c '<argv0>'` when
/// there is nothing to substitute).
fn shell_command(argv: &[String]) -> std::process::Command {
    const METACHARS: &[char] = &[
        '|', '&', ';', '<', '>', '(', ')', '$', '`', '\\', '"', '\'', ' ', '\t', '\n', '*', '?',
        '[', '#', '~', '=', '%',
    ];
    if !argv[0].contains(METACHARS) {
        let mut proc = std::process::Command::new(&argv[0]);
        proc.args(&argv[1..]);
        return proc;
    }
    let mut proc = std::process::Command::new("sh");
    proc.arg("-c");
    if argv.len() == 1 {
        proc.arg(&argv[0]);
    } else {
        proc.arg(format!("{} \"$@\"", argv[0]));
        // git pushes the whole argv after the script, so `$0` is the command
        // word itself and `"$@"` starts at the first real argument.
        proc.args(argv);
    }
    proc
}

/// git's `sq_quote_buf` (quote.c): always single-quote, escaping `'` and `!` by
/// closing the quote, backslash-escaping the character, and reopening.
fn sq_quote(src: &BStr) -> String {
    let mut out = BString::from("'");
    for &b in src.iter() {
        if b == b'\'' || b == b'!' {
            out.extend_from_slice(b"'\\");
            out.push(b);
            out.push(b'\'');
        } else {
            out.push(b);
        }
    }
    out.push(b'\'');
    out.to_str_lossy().into_owned()
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
    let modules = read_gitmodules(repo);

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let config_path = repo.common_dir().join("config");
    let mut config = ConfigFile::from_path_no_includes(config_path.clone(), Source::Local)?;
    let mut dirty = false;

    for entry in &entries {
        let Some(sub) = find_submodule(&submodules, &entry.path) else {
            continue;
        };
        let display = match super_prefix {
            Some(sp) => format!("{sp}{}", entry.path),
            None => display_path(entry.path.as_bstr(), prefix),
        };
        let outcome = sync_one(
            repo,
            &index,
            sub,
            &entry.path,
            &display,
            quiet,
            recursive,
            modules.as_ref(),
            &mut config,
        )?;
        match outcome {
            SyncOne::Skipped => continue,
            SyncOne::Synced => dirty = true,
            SyncOne::Failed(code) => {
                persist(&config_path, &config)?;
                return Ok(code);
            }
        }
    }

    if dirty {
        persist(&config_path, &config)?;
    }
    Ok(0)
}

/// What [`sync_one`] did, so its caller knows whether the superproject config
/// still needs writing out.
enum SyncOne {
    /// Inactive: git's `sync_submodule` returns before touching anything.
    Skipped,
    /// `submodule.<name>.url` was set in the caller's config file.
    Synced,
    /// A recursive descent failed with this exit code; the config was written.
    Failed(u8),
}

/// git's `sync_submodule` (submodule--helper.c:1429) for one gitlink: copy the
/// `.gitmodules` url into the superproject's `submodule.<name>.url`, and — when
/// the submodule is populated — into `remote.<default-remote>.url` inside the
/// submodule's own config, descending afterwards under `--recursive`.
///
/// The superproject config is written by the caller: git rewrites `.git/config`
/// once per submodule, and batching the writes is the only difference.
#[allow(clippy::too_many_arguments)]
fn sync_one(
    repo: &gix::Repository,
    index: &gix::index::State,
    sub: &gix::Submodule<'_>,
    path: &BString,
    display: &str,
    quiet: bool,
    recursive: bool,
    modules: Option<&ConfigFile>,
    config: &mut ConfigFile,
) -> Result<SyncOne> {
    // `sync_submodule` returns immediately for an inactive submodule.
    if !is_submodule_active(repo, index, sub, path)? {
        return Ok(SyncOne::Skipped);
    }
    let sub_name = sub.name().to_owned();
    let sub_name = sub_name.as_bstr();

    // The url git copies to both the superproject and the submodule remote.
    // A relative url needs `resolve_relative_url` against the superproject's
    // default remote, which is not ported — bail rather than register a
    // literal `./`/`../` url git would have rewritten.
    let url = modules.and_then(|m| m.string_by("submodule", Some(sub_name), "url"));
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

    // `is_submodule_populated_gently`: no repository on disk means git stops
    // here (the remote-url rewrite and any recursion are skipped).
    let sub_repo = repo
        .workdir()
        .and_then(|wd| gix::open(wd.join(&*gix::path::from_bstr(path.as_bstr()))).ok());
    let Some(sub_repo) = sub_repo else {
        return Ok(SyncOne::Synced);
    };

    // Rewrite `remote.<default-remote>.url` in the submodule's own config.
    let remote = BString::from(default_remote(&sub_repo)?);
    let sub_config_path = sub_repo.common_dir().join("config");
    {
        let _sub_lock = crate::lock::RepoLock::acquire(sub_repo.git_dir());
        let mut sub_config =
            ConfigFile::from_path_no_includes(sub_config_path.clone(), Source::Local)?;
        sub_config.set_raw_value_by("remote", Some(remote.as_bstr()), "url", url_bytes.as_bstr())?;
        persist(&sub_config_path, &sub_config)?;
    }

    if recursive {
        let nested = format!("{display}/");
        let code = sync_repo(&sub_repo, &[], quiet, true, Some(&nested), None)?;
        if code != 0 {
            return Ok(SyncOne::Failed(code));
        }
    }
    Ok(SyncOne::Synced)
}

/// The worktree `.gitmodules` parsed raw, so urls are read verbatim — exactly as
/// git's `sub->url` is the literal string from the file, never a round-tripped
/// URL. `None` when the file is absent or unparsable, which git treats as "no
/// mappings".
fn read_gitmodules(repo: &gix::Repository) -> Option<ConfigFile> {
    ConfigFile::from_path_no_includes(gitmodules_path(repo), Source::Local).ok()
}

/// Where the worktree `.gitmodules` lives, falling back to the current directory
/// for a bare repository (which git's `is_writing_gitmodules_ok` would refuse).
fn gitmodules_path(repo: &gix::Repository) -> std::path::PathBuf {
    match repo.workdir() {
        Some(wd) => wd.join(".gitmodules"),
        None => std::path::PathBuf::from(".gitmodules"),
    }
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

// --------------------------------------------------------------- set-url ----

/// The `usage:` block `module_set_url`'s `usage_with_options` prints (exit 129),
/// captured byte-for-byte from git 2.55.0 (`git submodule--helper set-url`).
const SET_URL_USAGE: &str = "\
usage: git submodule set-url [--quiet] <path> <newurl>

    -q, --[no-]quiet      suppress output for setting url of a submodule

";

/// `git submodule set-url [--quiet] [--] <path> <newurl>` — git's
/// `module_set_url` (submodule--helper.c:3228).
///
/// Writes `submodule.<name>.url` into the worktree `.gitmodules`, keyed by the
/// submodule *name* the `<path>` maps to, then re-reads the file and runs the
/// same `sync_submodule` that `git submodule sync` runs for that one submodule —
/// which is what copies the new url into `.git/config` and into the submodule's
/// own `remote.<default-remote>.url`, and what prints the
/// `Synchronizing submodule url for '<displaypath>'` line.
///
/// `<path>` is matched against `.gitmodules` verbatim, not joined with the cwd
/// prefix: git's `submodule_from_path(the_repository, null_oid, argv[0])` looks
/// the raw operand up against the root-relative `path` field, so from a
/// subdirectory `set-url sub <url>` is the one that resolves and `set-url ../sub
/// <url>` is the one that dies (confirmed against git 2.55.0).
fn set_url(args: &[String], mut quiet: bool) -> Result<ExitCode> {
    let mut operands: Vec<String> = Vec::new();
    let mut no_more_opts = false;

    // `cmd_set_url` filters the option list before the helper sees it: only
    // `-q`/`--quiet` and `--` pass, and any other `-*` is the porcelain's own
    // `usage` (exit 1). The first operand does not stop the scan — the shell
    // loop `break`s on it and forwards the remainder after `--`.
    for a in args {
        if no_more_opts {
            operands.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => no_more_opts = true,
            "-q" | "--quiet" => quiet = true,
            s if s.starts_with('-') && s.len() > 1 => return usage_exit(),
            _ => {
                operands.push(a.clone());
                no_more_opts = true;
            }
        }
    }

    if operands.len() != 2 {
        eprint!("{SET_URL_USAGE}");
        return Ok(ExitCode::from(129));
    }
    let (path, newurl) = (BString::from(operands[0].as_str()), operands[1].clone());

    let repo = gix::discover(".")?;
    let prefix = repo_prefix(&repo)?;
    let submodules = submodules(&repo)?;
    let Some(sub) = find_submodule(&submodules, &path) else {
        eprintln!("fatal: no submodule mapping found in .gitmodules for path '{path}'");
        return Ok(ExitCode::from(128));
    };
    let sub_name = sub.name().to_owned();

    // `config_set_in_gitmodules_file_gently`; a failure here returns 1 and the
    // sync below never runs.
    let modules_path = gitmodules_path(&repo);
    {
        let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
        let mut modules = ConfigFile::from_path_no_includes(modules_path.clone(), Source::Local)?;
        modules.set_raw_value_by(
            "submodule",
            Some(sub_name.as_bstr()),
            "url",
            newurl.as_str(),
        )?;
        persist(&modules_path, &modules)?;
    }

    // `repo_read_gitmodules(the_repository, 0)` then `sync_submodule(sub->path,
    // prefix, NULL, ...)`: the sync must see the url just written.
    let index = repo.open_index()?;
    let display = display_path(path.as_bstr(), prefix.as_ref());
    let modules = read_gitmodules(&repo);
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let config_path = repo.common_dir().join("config");
    let mut config = ConfigFile::from_path_no_includes(config_path.clone(), Source::Local)?;
    let outcome = sync_one(
        &repo,
        &index,
        sub,
        &path,
        &display,
        quiet,
        false,
        modules.as_ref(),
        &mut config,
    )?;
    Ok(match outcome {
        SyncOne::Skipped => ExitCode::SUCCESS,
        SyncOne::Synced => {
            persist(&config_path, &config)?;
            ExitCode::SUCCESS
        }
        SyncOne::Failed(code) => {
            persist(&config_path, &config)?;
            ExitCode::from(code)
        }
    })
}

// ---------------------------------------------------------------- deinit ----

/// The `usage:` block `module_deinit`'s `usage_with_options` prints (exit 129),
/// captured byte-for-byte from git 2.55.0
/// (`git submodule--helper deinit --all <path>`).
const DEINIT_USAGE: &str = "\
usage: git submodule deinit [--quiet] [-f | --force] [--all | [--] [<path>...]]

    -q, --[no-]quiet      suppress submodule status output
    -f, --[no-]force      remove submodule working trees even if they contain local changes
    --[no-]all            unregister all submodules

";

/// `git submodule deinit [-q] [-f] (--all | [--] <path>...)` — git's
/// `module_deinit` (submodule--helper.c:1677).
fn deinit(args: &[String], mut quiet: bool) -> Result<ExitCode> {
    let mut force = false;
    let mut all = false;
    let mut patterns: Vec<BString> = Vec::new();
    let mut no_more_opts = false;

    // `cmd_deinit`'s option loop: `-f`/`--force`, `-q`/`--quiet`, `--all`, `--`;
    // the first operand breaks out and any other `-*` is the porcelain's usage.
    for a in args {
        if no_more_opts {
            patterns.push(BString::from(a.as_str()));
            continue;
        }
        match a.as_str() {
            "--" => no_more_opts = true,
            "-q" | "--quiet" => quiet = true,
            "-f" | "--force" => force = true,
            "--all" => all = true,
            s if s.starts_with('-') && s.len() > 1 => return usage_exit(),
            _ => {
                patterns.push(BString::from(a.as_str()));
                no_more_opts = true;
            }
        }
    }

    if all && !patterns.is_empty() {
        eprintln!("error: pathspec and --all are incompatible");
        eprint!("{DEINIT_USAGE}");
        return Ok(ExitCode::from(129));
    }
    if patterns.is_empty() && !all {
        eprintln!("fatal: Use '--all' if you really want to deinitialize all submodules");
        return Ok(ExitCode::from(128));
    }
    if let Some(code) = reject_empty_pathspec(&patterns) {
        return Ok(code);
    }

    let repo = gix::discover(".")?;
    let prefix = repo_prefix(&repo)?;
    let index = repo.open_index()?;
    let entries = match module_list(&repo, &index, &patterns)? {
        Ok(entries) => entries,
        Err(code) => return Ok(ExitCode::from(code)),
    };
    let submodules = submodules(&repo)?;
    let modules = read_gitmodules(&repo);
    let Some(workdir) = repo.workdir().map(ToOwned::to_owned) else {
        return Ok(ExitCode::SUCCESS);
    };

    for entry in &entries {
        // `if (!sub || !sub->name) goto cleanup` — a gitlink with no
        // `.gitmodules` mapping is silently left alone.
        let Some(sub) = find_submodule(&submodules, &entry.path) else {
            continue;
        };
        deinit_one(
            &repo,
            &workdir,
            sub,
            &entry.path,
            &display_path(entry.path.as_bstr(), prefix.as_ref()),
            modules.as_ref(),
            quiet,
            force,
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

/// git's `deinit_submodule` (submodule--helper.c:1578) for one gitlink: empty the
/// worktree directory and drop the whole `submodule.<name>` section from
/// `.git/config`, leaving `.gitmodules` and the gitlink itself untouched.
#[allow(clippy::too_many_arguments)]
fn deinit_one(
    repo: &gix::Repository,
    workdir: &std::path::Path,
    sub: &gix::Submodule<'_>,
    path: &BString,
    display: &str,
    modules: Option<&ConfigFile>,
    quiet: bool,
    force: bool,
) -> Result<()> {
    let sub_name = sub.name().to_owned();
    let sm_dir = workdir.join(&*gix::path::from_bstr(path.as_bstr()));

    if sm_dir.is_dir() {
        // A real `.git` *directory* inside the worktree would be deleted along
        // with it, so git relocates it into the superproject first.
        if sm_dir.join(".git").is_dir() {
            if !quiet {
                eprintln!(
                    "warning: Submodule work tree '{display}' contains a .git directory. This will be replaced with a .git file by using absorbgitdirs."
                );
            }
            absorb_one(repo, path, None)?;
        }

        // `git rm -qn <path>`: a dry run whose only job is to refuse when the
        // worktree carries changes that removing it would throw away.
        if !force {
            let dry = crate::dispatch::run(
                "rm",
                &["-qn".to_string(), path.to_str_lossy().into_owned()],
            )?;
            if dry != ExitCode::SUCCESS {
                crate::git_fatal!(
                    "Submodule work tree '{display}' contains local modifications; use '-f' to discard them"
                );
            }
        }

        let removed = std::fs::remove_dir_all(&sm_dir).is_ok();
        if !quiet {
            if removed {
                println!("Cleared directory '{display}'");
            } else {
                println!("Could not remove submodule work tree '{display}'");
            }
            std::io::stdout().flush()?;
        }
        unset_core_worktree(repo, sub_name.as_bstr(), display);
    }

    // git recreates the (now empty) directory so the gitlink keeps a mount
    // point. The diagnostic it prints when that fails carries no newline.
    if std::fs::create_dir(&sm_dir).is_err() {
        print!("could not create empty submodule directory {display}");
        std::io::stdout().flush()?;
    }

    // `git config --get-regexp "submodule.<name>\."` decides whether there is
    // anything registered; only then is the section removed and the line
    // printed. git drops the *whole* section so a later `init` starts clean.
    let config_path = repo.common_dir().join("config");
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let mut config = ConfigFile::from_path_no_includes(config_path.clone(), Source::Local)?;
    let mut removed_any = false;
    while config
        .remove_section("submodule", Some(sub_name.as_bstr()))
        .is_some()
    {
        removed_any = true;
    }
    if removed_any {
        persist(&config_path, &config)?;
        if !quiet {
            let url = modules
                .and_then(|m| m.string_by("submodule", Some(sub_name.as_bstr()), "url"))
                .unwrap_or_default();
            println!(
                "Submodule '{}' ({}) unregistered for path '{display}'",
                sub_name,
                url.to_str_lossy()
            );
            std::io::stdout().flush()?;
        }
    }
    Ok(())
}

/// git's `submodule_unset_core_worktree` (submodule.c:2059): drop
/// `core.worktree` from the submodule's own config, warning rather than dying
/// when that cannot be done.
///
/// "Cannot be done" includes *there was nothing to unset*: git goes through
/// `repo_config_set_in_file_gently(..., NULL)`, which answers `CONFIG_NOTHING_SET`
/// for an absent key, so a second `deinit` of the same submodule warns even
/// though the first one already left the config in the wanted state.
fn unset_core_worktree(repo: &gix::Repository, name: &BStr, display: &str) {
    let unset = || -> Result<bool> {
        let path = submodule_name_to_gitdir(repo, name)?.join("config");
        let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
        let mut config = ConfigFile::from_path_no_includes(path.clone(), Source::Local)?;
        let removed = config
            .section_mut("core", None)
            .ok()
            .and_then(|mut section| section.remove("worktree"))
            .is_some();
        if removed {
            persist(&path, &config)?;
        }
        Ok(removed)
    };
    if !matches!(unset(), Ok(true)) {
        eprintln!("warning: Could not unset core.worktree setting in submodule '{display}'");
    }
}

// --------------------------------------------------------- absorbgitdirs ----

/// The `usage:` block `absorb_git_dirs`' `usage_with_options` prints (exit 129).
/// Its only option, `--super-prefix`, is `PARSE_OPT_HIDDEN`, so the block is the
/// usage line and a blank line and nothing else.
const ABSORB_USAGE: &str = "usage: git submodule absorbgitdirs [<options>] [<path>...]\n\n";

/// `git submodule absorbgitdirs [--] [<path>...]` — git's `absorb_git_dirs`
/// (submodule--helper.c:3194). `cmd_absorbgitdirs` forwards its arguments
/// unfiltered, so the porcelain and the helper parse identically here.
fn absorbgitdirs(args: &[String]) -> Result<ExitCode> {
    let mut patterns: Vec<BString> = Vec::new();
    let mut super_prefix: Option<String> = None;
    let mut no_more_opts = false;

    let mut i = 0;
    while let Some(a) = args.get(i) {
        i += 1;
        if no_more_opts {
            patterns.push(BString::from(a.as_str()));
            continue;
        }
        match a.as_str() {
            "--" => no_more_opts = true,
            "--super-prefix" => match args.get(i) {
                Some(v) => {
                    super_prefix = Some(v.clone());
                    i += 1;
                }
                None => {
                    eprintln!("error: option `super-prefix' requires a value");
                    eprint!("{ABSORB_USAGE}");
                    return Ok(ExitCode::from(129));
                }
            },
            s if s.starts_with("--super-prefix=") => {
                super_prefix = Some(s["--super-prefix=".len()..].to_string());
            }
            s if s.starts_with("--") => {
                eprintln!("error: unknown option `{}'", &s[2..]);
                eprint!("{ABSORB_USAGE}");
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with('-') && s.len() > 1 => {
                eprintln!(
                    "error: unknown switch `{}'",
                    s[1..].chars().next().expect("len > 1")
                );
                eprint!("{ABSORB_USAGE}");
                return Ok(ExitCode::from(129));
            }
            _ => patterns.push(BString::from(a.as_str())),
        }
    }

    if let Some(code) = reject_empty_pathspec(&patterns) {
        return Ok(code);
    }

    let repo = gix::discover(".")?;
    let code = absorb_repo(&repo, &patterns, super_prefix.as_deref())?;
    Ok(ExitCode::from(code))
}

/// One superproject's worth of `absorbgitdirs`: `absorb_git_dir_into_superproject`
/// for every listed gitlink.
fn absorb_repo(
    repo: &gix::Repository,
    patterns: &[BString],
    super_prefix: Option<&str>,
) -> Result<u8> {
    let index = repo.open_index()?;
    let entries = match module_list(repo, &index, patterns)? {
        Ok(entries) => entries,
        Err(code) => return Ok(code),
    };
    for entry in &entries {
        absorb_one(repo, &entry.path, super_prefix)?;
    }
    Ok(0)
}

/// git's `absorb_git_dir_into_superproject` (submodule.c:2556): make the
/// submodule's repository live under the superproject's `modules/<name>`, with
/// `<path>/.git` a `gitdir:` file pointing at it, then recurse.
fn absorb_one(
    repo: &gix::Repository,
    path: &BString,
    super_prefix: Option<&str>,
) -> Result<()> {
    let Some(workdir) = repo.workdir() else {
        return Ok(());
    };
    let sm_dir = workdir.join(&*gix::path::from_bstr(path.as_bstr()));
    let dot_git = sm_dir.join(".git");

    // `resolve_gitdir_gently(<path>/.git, &err_code)`.
    if !dot_git.exists() {
        // `READ_GITFILE_ERR_MISSING`: unpopulated as expected, and git returns
        // *before* recursing.
        return Ok(());
    }
    match gix::open(&sm_dir) {
        Ok(sub_repo) => {
            // Already absorbed? git compares the resolved git dir against the
            // superproject's resolved common dir by prefix.
            let real_sub = real_path(sub_repo.git_dir());
            let real_common = real_path(repo.common_dir());
            if !real_sub.starts_with(&real_common) {
                relocate_git_dir(repo, path, &sm_dir, super_prefix)?;
            }
        }
        Err(_) => {
            // `READ_GITFILE_ERR_NOT_A_REPO`: populated, but the gitfile points
            // nowhere — the superproject was itself just absorbed and the link
            // has not been rewritten yet. Repoint it at where it must live.
            let submodules = submodules(repo)?;
            let Some(sub) = find_submodule(&submodules, path) else {
                crate::git_fatal!("could not lookup name for submodule '{path}'");
            };
            let sub_gitdir = submodule_name_to_gitdir(repo, sub.name())?;
            connect_work_tree_and_git_dir(&sm_dir, &sub_gitdir)?;
        }
    }

    // `absorb_git_dir_into_superproject_recurse`: the same pass inside the
    // submodule, with the display prefix extended by this path.
    let Ok(sub_repo) = gix::open(&sm_dir) else {
        crate::git_fatal!("could not recurse into submodule '{path}'");
    };
    let nested = format!("{}{path}/", super_prefix.unwrap_or(""));
    absorb_repo(&sub_repo, &[], Some(&nested))?;
    Ok(())
}

/// git's `relocate_single_git_dir_into_superproject` (submodule.c:2487): move a
/// `<path>/.git` *directory* into `<superproject-git-dir>/modules/<name>` and
/// leave the pair of links behind.
fn relocate_git_dir(
    repo: &gix::Repository,
    path: &BString,
    sm_dir: &std::path::Path,
    super_prefix: Option<&str>,
) -> Result<()> {
    let old_git_dir = sm_dir.join(".git");
    // An actual gitfile does not need migration; only a real directory does.
    if old_git_dir.is_file() {
        return Ok(());
    }
    // `submodule_uses_worktrees`: `relocate_gitdir` would break the linked
    // worktrees' `gitdir` files, so git refuses outright.
    if std::fs::read_dir(old_git_dir.join("worktrees"))
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
    {
        crate::git_fatal!(
            "relocate_gitdir for submodule '{path}' with more than one worktree not supported"
        );
    }

    let real_old = real_path(&old_git_dir);
    let submodules = submodules(repo)?;
    let Some(sub) = find_submodule(&submodules, path) else {
        crate::git_fatal!("could not lookup name for submodule '{path}'");
    };
    let new_git_dir = submodule_name_to_gitdir(repo, sub.name())?;
    if let Some(parent) = new_git_dir.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            crate::git_fatal!("could not create directory '{}'", new_git_dir.display());
        }
    }
    let real_new = real_path(&new_git_dir);

    eprintln!(
        "Migrating git directory of '{}{path}' from\n'{}' to\n'{}'",
        super_prefix.unwrap_or(""),
        real_old.display(),
        real_new.display()
    );

    // `relocate_gitdir`: rename, then re-link the worktree and the git dir.
    if let Err(err) = std::fs::rename(&real_old, &real_new) {
        crate::git_fatal!(
            "could not migrate git directory from '{}' to '{}': {err}",
            real_old.display(),
            real_new.display()
        );
    }
    connect_work_tree_and_git_dir(sm_dir, &real_new)
}

/// git's `real_pathdup(path, 1)`: the path with every symlink resolved. A
/// component that does not exist yet cannot be resolved, so the deepest existing
/// ancestor is resolved and the remainder appended — which is what git's
/// `strbuf_realpath` does for the not-yet-created `modules/<name>`.
fn real_path(path: &std::path::Path) -> std::path::PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => real_path(parent).join(name),
        _ => absolute(path),
    }
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

/// The flags of `git submodule update` this port honors. `--filter` (the
/// partial-clone shaping flag) still bails in `update` before any repository is
/// touched, so it never reaches here.
#[derive(Clone)]
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
    /// git's `max_jobs`: how many submodule clones may run at once. `UPDATE_DATA_INIT`
    /// starts at 1; `.gitmodules`' then the repository's `submodule.fetchJobs` raise it,
    /// and `-j`/`--jobs` overrides both.
    jobs: usize,
    /// `--depth <n>`, git's `update_data->depth`: the shallow depth a missing
    /// submodule is cloned at. `0`/`None` is unset.
    depth: Option<u32>,
    /// `--recommend-shallow` / `--no-recommend-shallow`, git's tri-state
    /// `recommend_shallow` whose `UPDATE_DATA_INIT` value is `-1`. Since `-1` is
    /// truthy in C, only an explicit `--no-recommend-shallow` switches it off; when
    /// it is on, a submodule whose `.gitmodules` carries `shallow = true` is cloned
    /// with `--depth=1` *ahead of* an explicit `--depth`
    /// (`prepare_to_clone_next_submodule`, submodule--helper.c:2349-2352).
    recommend_shallow: Option<bool>,
    /// `--reference <repo>`, repeatable: reference repositories forwarded to the
    /// submodule's clone as `--reference`.
    references: Vec<String>,
    /// `--dissociate`: forwarded to the submodule's clone.
    dissociate: bool,
    /// `--single-branch` / `--no-single-branch`, git's tri-state `single_branch`
    /// (`-1` forwards nothing).
    single_branch: Option<bool>,
    /// `--require-init`: refuse to clone into a worktree directory that is not
    /// empty, and refuse to keep one that gained anything besides `.git`.
    require_init: bool,
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
        // `UPDATE_DATA_INIT`'s `.max_jobs = 1`, before the config below raises it.
        jobs: 1,
        depth: None,
        recommend_shallow: None,
        references: Vec::new(),
        dissociate: false,
        single_branch: None,
        require_init: false,
    };
    let mut patterns: Vec<BString> = Vec::new();
    let mut no_more_opts = false;
    // git reads the config into `max_jobs` before `parse_options`, so a command-line `-j`
    // always wins; this repo is only discovered after parsing, hence the flag.
    let mut jobs_from_cli = false;
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
            // Progress forcing has no effect here: the clone children are piped, so their
            // progress is off either way, and nothing else in `update` reports progress.
            "--progress" => {}
            // `OPT_INTEGER('j', "jobs", &opt.max_jobs, ...)`: `-j5`, `-j 5`, `--jobs 5` and
            // `--jobs=5` all set the same slot.
            "-j" | "--jobs" => {
                let Some(v) = args.get(i + 1) else {
                    return usage_exit();
                };
                opts.jobs = parse_jobs(a, v)?;
                jobs_from_cli = true;
                i += 1;
            }
            s if s.starts_with("--jobs=") => {
                opts.jobs = parse_jobs("--jobs", &s["--jobs=".len()..])?;
                jobs_from_cli = true;
            }
            s if s.starts_with("-j") && s.len() > 2 => {
                opts.jobs = parse_jobs("-j", &s[2..])?;
                jobs_from_cli = true;
            }
            // The clone-shaping options, all of which `clone_submodule` forwards to
            // the `git clone` it runs for a missing submodule.
            "--reference" => {
                let Some(v) = args.get(i + 1) else {
                    return usage_exit();
                };
                opts.references.push(v.clone());
                i += 1;
            }
            s if s.starts_with("--reference=") => {
                opts.references.push(s["--reference=".len()..].to_owned());
            }
            "--dissociate" => opts.dissociate = true,
            "--no-dissociate" => opts.dissociate = false,
            "--depth" => {
                let Some(v) = args.get(i + 1) else {
                    return usage_exit();
                };
                opts.depth = parse_depth(v)?;
                i += 1;
            }
            s if s.starts_with("--depth=") => opts.depth = parse_depth(&s["--depth=".len()..])?,
            "--recommend-shallow" => opts.recommend_shallow = Some(true),
            "--no-recommend-shallow" => opts.recommend_shallow = Some(false),
            "--single-branch" => opts.single_branch = Some(true),
            "--no-single-branch" => opts.single_branch = Some(false),
            "--require-init" => opts.require_init = true,
            // Partial clone still needs machinery this port does not carry, so it
            // bails rather than clone a submodule without the filter it asked for.
            s if s.starts_with("--filter=") => bail!(
                "`submodule update {s}` shapes the partial-clone fetch, which is not ported"
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

    // `module_update`: `--require-init` turns the registration pass on by itself
    // (submodule--helper.c:3049-3050), which is why `git clone --recurse-submodules`
    // never passes `--init` alongside it.
    if opts.require_init {
        opts.init = true;
    }

    if let Some(code) = reject_empty_pathspec(&patterns) {
        return Ok(code);
    }

    let repo = gix::discover(".")?;
    // `update_clone_config_from_gitmodules()` then `git_update_clone_config()`: `.gitmodules`
    // supplies the default and the repository configuration overrides it, both before the
    // command line is parsed — so an explicit `-j` still wins.
    if !jobs_from_cli {
        if let Some(n) = submodule_fetch_jobs(&repo)? {
            opts.jobs = n;
        }
    }
    let prefix = repo_prefix(&repo)?;
    // `warn_if_uninitialized` is set only when a pathspec was given.
    let warn = !patterns.is_empty();
    let code = update_repo(repo, &patterns, &opts, None, prefix.as_ref(), warn)?;
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
    opts: &UpdateOpts,
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

    // git's `update_submodules` runs in two phases: `run_processes_parallel` clones every
    // submodule that needs one, up to `max_jobs` at a time, and only then does a serial pass
    // check each one out. The task *generator*
    // (`update_clone_get_next_task` -> `prepare_to_clone_next_submodule`) still runs serially, so
    // the skip ladder below and its messages keep git's order regardless of the job count.
    let mut plans: Vec<Plan<'_>> = Vec::new();
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

        if !is_submodule_active(&repo, &index, sub, &entry.path)? {
            warn_missing(warn, &display);
            continue;
        }

        // git's `prepare_to_clone_next_submodule` treats a missing `<path>/.git` as
        // "needs cloning"; a just-cloned submodule then gets `suboid = null` and a
        // forced checkout of the recorded commit. git's `clone_data_path` is the
        // absolute `<worktree>/<path>`, and it reaches the clone child verbatim —
        // so the "Cloning into '…'" line names an absolute path.
        let sm_dir = absolute(&workdir.join(&*gix::path::from_bstr(entry.path.as_bstr())));
        let sub_name = sub.name().to_owned();
        let clone_url = if sm_dir.join(".git").exists() {
            None
        } else {
            // git reads `submodule.<name>.url` from config (an `init`/`update --init`
            // pass writes it there), falling back to a relative `.gitmodules` url via
            // `resolve_relative_url` — which is not ported, so a `./`/`../` url bails.
            let url = repo.config_snapshot().string(key(sub_name.as_bstr(), "url"));
            let Some(url) = url else {
                crate::git_fatal!(
                    "submodule '{display}' has no registered `submodule.{sub_name}.url`; run `submodule init` (or `update --init`) first"
                );
            };
            if url.starts_with(b"./") || url.starts_with(b"../") {
                bail!(
                    "submodule '{sub_name}' has the relative url {:?}; resolving it against the default remote is not ported",
                    url.to_str_lossy()
                );
            }
            Some(url.to_owned())
        };

        plans.push(Plan {
            entry,
            display,
            sm_dir,
            // `clone_submodule_sm_gitdir()`: the repository itself lives outside the
            // worktree, under the superproject's `modules/` directory.
            sm_gitdir: submodule_name_to_gitdir(&repo, sub_name.as_bstr())?,
            // `sub->recommend_shallow`, i.e. `submodule.<name>.shallow` in
            // `.gitmodules`; git only honours the `== 1` case.
            recommend_shallow: sub.shallow()?.unwrap_or(false),
            sub_name,
            cfg_strategy,
            clone_url,
        });
    }

    // Phase one: the clones, `opts.jobs` at a time. git buffers each child's output and
    // replays it in task order once the phase is over ("We saved the output and put it out
    // all at once now"), which is what keeps the transcript identical at any job count.
    let alternates = AlternateSetup::from_repo(&repo);
    if let Some(code) = clone_submodules_in_parallel(&plans, &workdir, &alternates, opts)? {
        return Ok(code);
    }

    for plan in &plans {
        let Plan {
            entry,
            display,
            sm_dir,
            cfg_strategy,
            clone_url,
            ..
        } = plan;
        let (display, sm_dir, cfg_strategy) = (display.as_str(), sm_dir.as_path(), cfg_strategy.clone());
        let just_cloned = clone_url.is_some();

        let Ok(sub_repo) = gix::open(sm_dir) else {
            crate::git_fatal!("submodule path '{display}' could not be opened after cloning");
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
            let sub = find_submodule(&submodules, &entry.path)
                .expect("the planning pass already found this declaration");
            match resolve_remote_oid(&repo, &sub_repo, sub, entry, sm_dir, opts)? {
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
                run_update_procedure(&sub_repo, sm_dir, &oid, opts, display, strategy, subforce)?;
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

/// `OPT_INTEGER`'s value for `-j`/`--jobs`.
///
/// Unlike the config key below this is a plain integer with no `0` special case, so a `0` reaches
/// `run_processes_parallel` and trips its `you must provide a non-zero number of processes!`
/// assertion; refuse it here rather than substitute a count git never picks.
fn parse_jobs(flag: &str, value: &str) -> Result<usize> {
    let n: i64 = value
        .parse()
        .map_err(|_| anyhow::anyhow!("{flag} expects a numerical value"))?;
    if n <= 0 {
        crate::git_fatal!("you must provide a non-zero number of processes");
    }
    Ok(n as usize)
}

/// `OPT_INTEGER`'s value for `--depth`. git stores it in an `int` and only tests
/// `depth > 0` before forwarding it, so `0` and a negative value both mean "no
/// `--depth` on the child" rather than an error.
fn parse_depth(value: &str) -> Result<Option<u32>> {
    let n: i64 = value
        .parse()
        .map_err(|_| anyhow::anyhow!("--depth expects a numerical value"))?;
    Ok(u32::try_from(n).ok().filter(|n| *n > 0))
}

/// `parse_submodule_fetchjobs()`: a negative count is fatal and `0` means one job per CPU.
fn parse_fetch_jobs_config(value: &str) -> Result<usize> {
    let n: i64 = value
        .parse()
        .map_err(|_| anyhow::anyhow!("bad numeric config value {value:?} for 'submodule.fetchjobs'"))?;
    if n < 0 {
        crate::git_fatal!("negative values not allowed for submodule.fetchJobs");
    }
    Ok(if n == 0 {
        std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
    } else {
        n as usize
    })
}

/// `submodule.fetchJobs` as `update` reads it: the `.gitmodules` value first, then the
/// repository configuration on top (`update_clone_config_from_gitmodules()` followed by
/// `git_update_clone_config()`). `None` means neither set it.
fn submodule_fetch_jobs(repo: &gix::Repository) -> Result<Option<usize>> {
    let mut jobs = None;
    if let Some(modules) = repo.open_modules_file()? {
        if let Some(v) = modules.config().string("submodule.fetchJobs") {
            jobs = Some(parse_fetch_jobs_config(&v.to_str_lossy())?);
        }
    }
    if let Some(v) = repo.config_snapshot().string("submodule.fetchJobs") {
        jobs = Some(parse_fetch_jobs_config(&v.to_str_lossy())?);
    }
    Ok(jobs)
}

/// One submodule that survived `prepare_to_clone_next_submodule`'s skip ladder, carried from
/// the serial planning pass into the parallel clone phase and the serial update pass.
struct Plan<'a> {
    entry: &'a Entry,
    display: String,
    /// The absolute `<worktree>/<path>` — git's `clone_data_path`.
    sm_dir: std::path::PathBuf,
    /// The absolute `<git-dir>/modules/<name>` this submodule's repository lives in,
    /// git's `sm_gitdir`.
    sm_gitdir: std::path::PathBuf,
    /// The submodule's `.gitmodules` name, which is what `modules/` is keyed by and
    /// what the alternate computation needs.
    sub_name: BString,
    /// `submodule.<name>.shallow` in `.gitmodules`, git's `sub->recommend_shallow`.
    recommend_shallow: bool,
    cfg_strategy: gix::submodule::config::Update,
    /// `submodule.<name>.url` when `<path>/.git` is missing and a clone has to run first.
    clone_url: Option<BString>,
}

/// git's `run_processes_parallel` phase of `update_submodules`: clone every submodule whose
/// worktree has no `.git` yet, at most `opts.jobs` at a time.
///
/// Each child's output is captured and replayed in task order once the phase is over, which is
/// what git means by "We saved the output and put it out all at once now" — the transcript is
/// then the same at any job count.
///
/// A clone that fails is retried once — git's `update_clone_task_finished` appends the failed
/// entry to `failed_clones`, which `update_clone_get_next_task` then serves "as an extension of
/// the entry list". Only a second failure sets `quickstop`, and `module_update` turns that into
/// exit code 1 whatever the child's own status was.
fn clone_submodules_in_parallel(
    plans: &[Plan<'_>],
    toplevel: &std::path::Path,
    alternates: &AlternateSetup,
    opts: &UpdateOpts,
) -> Result<Option<u8>> {
    let tasks: Vec<&Plan<'_>> = plans.iter().filter(|p| p.clone_url.is_some()).collect();
    if tasks.is_empty() {
        return Ok(None);
    }

    let mut retry: Vec<&Plan<'_>> = Vec::new();
    for (task, out) in tasks.iter().zip(clone_batch(&tasks, toplevel, alternates, opts)?) {
        replay(&out)?;
        if out.code != 0 {
            eprintln!("Failed to clone '{}'. Retry scheduled", task.entry.path);
            retry.push(task);
        }
    }
    if retry.is_empty() {
        return Ok(None);
    }

    for (task, out) in retry.iter().zip(clone_batch(&retry, toplevel, alternates, opts)?) {
        replay(&out)?;
        if out.code != 0 {
            eprintln!("Failed to clone '{}' a second time, aborting", task.entry.path);
            return Ok(Some(1));
        }
    }
    Ok(None)
}

/// One `run_processes_parallel` pass over `tasks`, `opts.jobs` at a time, with every
/// child's output buffered so the replay can run in task order.
fn clone_batch(
    tasks: &[&Plan<'_>],
    toplevel: &std::path::Path,
    alternates: &AlternateSetup,
    opts: &UpdateOpts,
) -> Result<Vec<CloneOutput>> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = opts.jobs.min(tasks.len()).max(1);
    let slots: Vec<std::sync::Mutex<Option<Result<CloneOutput>>>> =
        (0..tasks.len()).map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let (next, slots) = (&next, &slots);
            scope.spawn(move || loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(task) = tasks.get(i) else { break };
                let url = task.clone_url.as_ref().expect("filtered to cloning tasks");
                *slots[i].lock().expect("no panic while cloning") =
                    Some(clone_submodule(task, url, toplevel, alternates, opts));
            });
        }
    });
    slots
        .into_iter()
        .map(|slot| slot.into_inner().expect("no panic while cloning").expect("every task was claimed"))
        .collect()
}

/// Write one buffered child's streams out on this process's own.
fn replay(out: &CloneOutput) -> Result<()> {
    std::io::Write::write_all(&mut std::io::stdout(), &out.stdout)?;
    std::io::Write::write_all(&mut std::io::stderr(), &out.stderr)?;
    Ok(())
}

/// What one buffered `clone` child produced.
struct CloneOutput {
    code: u8,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
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
    // git's `git fetch` child runs `transport_check_allowed()` before it dials,
    // and dies `fatal: transport '<type>' not allowed` when the policy refuses —
    // which is what stops a `file` submodule url unless `protocol.file.allow` is
    // relaxed. The ported `fetch` does not implement the allow-list, so the same
    // check is applied here instead: one layer above the child, with the same
    // message, the same exit code, and before any transfer can start.
    if let Some(kind) = refused_transport(sm_dir, direct.map(|(remote, _)| remote))? {
        eprintln!("fatal: transport '{kind}' not allowed");
        return Ok(crate::fatal::EXIT_FATAL);
    }

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

/// The transport name `git fetch` inside `sm_dir` would refuse, or `None` when
/// the fetch may proceed (including when there is no url to judge — `git fetch`
/// then fails on its own terms, with its own message).
///
/// `remote` names the remote the fetch will use; `None` means the default one,
/// git's `repo_get_default_remote`.
fn refused_transport(sm_dir: &std::path::Path, remote: Option<&str>) -> Result<Option<String>> {
    let Ok(repo) = gix::open(sm_dir) else {
        return Ok(None);
    };
    let name = match remote {
        Some(r) => r.to_string(),
        None => default_remote(&repo)?,
    };
    let snapshot = repo.config_snapshot();
    let url = snapshot.string(KeyRef {
        section_name: "remote",
        subsection_name: Some(BStr::new(name.as_bytes())),
        value_name: "url",
    });
    let Some(url) = url else { return Ok(None) };
    let Ok(url) = gix::url::parse(url.as_ref()) else {
        return Ok(None);
    };
    let kind = match url.scheme {
        gix::url::Scheme::File => "file".to_string(),
        gix::url::Scheme::Git => "git".to_string(),
        gix::url::Scheme::Ssh => "ssh".to_string(),
        gix::url::Scheme::Http => "http".to_string(),
        gix::url::Scheme::Https => "https".to_string(),
        gix::url::Scheme::Ext(ref name) => name.clone(),
    };
    Ok((!transport_allowed(&snapshot, &kind)?).then_some(kind))
}

/// git's `is_transport_allowed` (transport.c:1124), the CVE-2022-39253 policy,
/// read against the repository the fetch will run in.
///
/// `GIT_ALLOW_PROTOCOL`, when set, is an exhaustive colon-separated allow-list
/// and nothing else is consulted. Otherwise `protocol.<type>.allow` decides,
/// falling back to `protocol.allow`, then to the built-in defaults: `http`,
/// `https`, `git` and `ssh` are `always`, `ext` is `never`, and everything else
/// — `file` included — is `user`, i.e. allowed only when the url came off the
/// command line rather than out of a file the repository carries.
fn transport_allowed(snapshot: &gix::config::Snapshot<'_>, kind: &str) -> Result<bool> {
    if let Ok(list) = std::env::var("GIT_ALLOW_PROTOCOL") {
        return Ok(list.split(':').any(|entry| entry == kind));
    }
    // git's `parse_protocol_config`: anything but these three is fatal.
    let parse = |key: &str, value: &BStr| match value.to_str_lossy().to_ascii_lowercase().as_str() {
        "always" => Ok(TransportPolicy::Always),
        "never" => Ok(TransportPolicy::Never),
        "user" => Ok(TransportPolicy::User),
        other => Err(anyhow::anyhow!("unknown value for config '{key}': {other}")),
    };
    let key = format!("protocol.{kind}.allow");
    let policy = match snapshot.string(key.as_str()) {
        Some(v) => parse(&key, v.as_ref())?,
        None => match snapshot.string("protocol.allow") {
            Some(v) => parse("protocol.allow", v.as_ref())?,
            None => match kind {
                "http" | "https" | "git" | "ssh" => TransportPolicy::Always,
                "ext" => TransportPolicy::Never,
                _ => TransportPolicy::User,
            },
        },
    };
    Ok(match policy {
        TransportPolicy::Always => true,
        TransportPolicy::Never => false,
        // `git_env_bool("GIT_PROTOCOL_FROM_USER", 1)`; `submodule` clears it.
        TransportPolicy::User => {
            !matches!(std::env::var("GIT_PROTOCOL_FROM_USER").as_deref(), Ok("0"))
        }
    })
}

/// git's `protocol_allow_config` (transport.c:1066).
enum TransportPolicy {
    Never,
    User,
    Always,
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

/// git's `clone_submodule` (submodule--helper.c:1899-2032), by re-executing the
/// ported `git clone … --separate-git-dir <sm_gitdir> -- <url> <path>` with a clean
/// repository env, then wiring the two halves together.
///
/// git also passes `--no-checkout` here and leaves populating the worktree to the
/// `git checkout -f <oid>` that `run_update_command` runs next. This port does not:
/// its `checkout` cannot populate a worktree from the absent index a `--no-checkout`
/// clone leaves behind, so the clone checks out the remote's branch tip and the
/// later `checkout -f` moves it to the recorded commit. Same end state, one extra
/// worktree write.
///
/// The repository lands in the superproject's `modules/<name>`, never inside the
/// worktree: `<path>/.git` is a `gitdir:` *file* pointing at it and the gitdir's
/// `core.worktree` points back. That is what makes a submodule survive its
/// worktree being deleted, and it is what `submodule.alternateLocation` needs in
/// order to have anything to compute an alternate from.
///
/// When `sm_gitdir` already exists there is nothing to clone — git only clears the
/// stale index and re-connects, so a `deinit`ed submodule comes back without a
/// second transfer.
///
/// Returns the child's exit code (0 on success).
fn clone_submodule(
    plan: &Plan<'_>,
    url: &BString,
    toplevel: &std::path::Path,
    alternates: &AlternateSetup,
    opts: &UpdateOpts,
) -> Result<CloneOutput> {
    let (sm_dir, sm_gitdir) = (plan.sm_dir.as_path(), plan.sm_gitdir.as_path());
    let mut out = CloneOutput { code: 0, stdout: Vec::new(), stderr: Vec::new() };

    if !sm_gitdir.exists() {
        // `require_init && !is_empty_dir(path)` — refuse before any transfer runs.
        if opts.require_init && !is_empty_dir(sm_dir) {
            out.stderr = format!("fatal: directory not empty: '{}'\n", sm_dir.display()).into_bytes();
            out.code = 128;
            return Ok(out);
        }
        if let Some(parent) = sm_gitdir.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // `prepare_possible_alternates()` runs *before* the clone, because what it
        // computes are `--reference` arguments for it.
        let mut references = opts.references.clone();
        if let Err(err) = prepare_possible_alternates(alternates, plan.sub_name.as_bstr(), &mut references, &mut out)
        {
            out.stderr.extend_from_slice(err.as_bytes());
            out.code = 128;
            return Ok(out);
        }

        let exe = std::env::current_exe()?;
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("clone");
        if opts.quiet {
            cmd.arg("--quiet");
        }
        // `recommend_shallow && sub->recommend_shallow == 1` wins over `--depth`.
        if opts.recommend_shallow != Some(false) && plan.recommend_shallow {
            cmd.arg("--depth=1");
        } else if let Some(depth) = opts.depth {
            cmd.arg(format!("--depth={depth}"));
        }
        for reference in &references {
            cmd.arg("--reference").arg(reference);
        }
        if opts.dissociate {
            cmd.arg("--dissociate");
        }
        cmd.arg("--separate-git-dir").arg(sm_gitdir);
        match opts.single_branch {
            Some(true) => {
                cmd.arg("--single-branch");
            }
            Some(false) => {
                cmd.arg("--no-single-branch");
            }
            None => {}
        }
        cmd.arg("--").arg(url.to_str_lossy().as_ref()).arg(sm_dir);
        cmd.current_dir(toplevel)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_PREFIX");
        // Buffered rather than inherited: `run_processes_parallel` pipes every child so the
        // transcript stays in task order no matter how many run at once.
        let child = cmd.output()?;
        out.stdout.extend_from_slice(&child.stdout);
        out.stderr.extend_from_slice(&child.stderr);
        let code = child_code(child.status);
        if code != 0 {
            out.stderr.extend_from_slice(
                format!(
                    "fatal: clone of '{}' into submodule path '{}' failed\n",
                    url.to_str_lossy(),
                    sm_dir.display()
                )
                .as_bytes(),
            );
            out.code = 128;
            return Ok(out);
        }

        // git's racy re-check here — `dir_contains_only_dotgit()`, guarding against a
        // parallel process filling the worktree while the clone ran — is not ported,
        // because it presumes the `--no-checkout` above: this clone leaves a checked
        // out worktree, so the check could only ever fail. The half of `--require-init`
        // that decides anything, the empty-directory test before the transfer, is above.
    } else {
        if opts.require_init && !is_empty_dir(sm_dir) {
            out.stderr = format!("fatal: directory not empty: '{}'\n", sm_dir.display()).into_bytes();
            out.code = 128;
            return Ok(out);
        }
        std::fs::create_dir_all(sm_dir)?;
        // The index describes a worktree that the `git checkout -f <oid>` in
        // `run_update_command` is about to rebuild, so it is stale by construction.
        let _ = std::fs::remove_file(sm_gitdir.join("index"));
    }

    connect_work_tree_and_git_dir(sm_dir, sm_gitdir)?;

    // The two keys are copied into the submodule's own config so that a *recursive*
    // update below this one resolves its alternates the same way.
    let sub_config = sm_gitdir.join("config");
    if let Some(location) = &alternates.location {
        set_config_value(&sub_config, "submodule.alternateLocation", location)?;
    }
    if let Some(strategy) = &alternates.error_strategy {
        set_config_value(&sub_config, "submodule.alternateErrorStrategy", strategy)?;
    }

    Ok(out)
}

/// `connect_work_tree_and_git_dir()` (dir.c:4108-4146) without the nested recursion:
/// write the worktree's `.git` gitfile and the gitdir's `core.worktree`, both as
/// paths relative to each other so the pair can be moved as a unit.
fn connect_work_tree_and_git_dir(work_tree: &std::path::Path, git_dir: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(work_tree)?;
    std::fs::create_dir_all(git_dir)?;
    // `real_pathdup()` on both sides: the relative path between them is only
    // meaningful once symlinks are out of the way.
    let real_git_dir = std::fs::canonicalize(git_dir)?;
    let real_work_tree = std::fs::canonicalize(work_tree)?;
    std::fs::write(
        work_tree.join(".git"),
        format!(
            "gitdir: {}\n",
            super::worktree::relative_path(&real_git_dir, &real_work_tree).display()
        ),
    )?;
    set_config_value(
        &git_dir.join("config"),
        "core.worktree",
        &super::worktree::relative_path(&real_work_tree, &real_git_dir)
            .to_string_lossy(),
    )
}

/// `repo_config_set_in_file()` for one `<section>.<key>` in a specific config file.
fn set_config_value(path: &std::path::Path, key: &str, value: &str) -> Result<()> {
    let mut file = match gix::config::File::from_path_no_includes(path.to_owned(), gix::config::Source::Local) {
        Ok(file) => file,
        Err(_) => gix::config::File::new(gix::config::file::Metadata::from(gix::config::Source::Local)),
    };
    let (section, name) = key.split_once('.').expect("every key here is `<section>.<name>`");
    file.set_raw_value_by(section, None, name, value)?;
    std::fs::write(path, file.to_bstring())?;
    Ok(())
}

/// The two `submodule.alternate*` keys plus the superproject's own alternate object
/// directories — everything `prepare_possible_alternates()` reads, gathered once in
/// the serial planning pass so the parallel clone phase needs no repository handle.
struct AlternateSetup {
    /// `submodule.alternateLocation`. `None` means the whole feature is off.
    location: Option<String>,
    /// `submodule.alternateErrorStrategy`; git defaults it to `die`.
    error_strategy: Option<String>,
    /// The superproject's `objects/info/alternates` entries, absolute — git's
    /// `odb_for_each_alternate()` list.
    super_alternates: Vec<std::path::PathBuf>,
}

impl AlternateSetup {
    fn from_repo(repo: &gix::Repository) -> Self {
        let config = repo.config_snapshot();
        let string = |key: &str| config.string(key).map(|v| v.to_str_lossy().into_owned());
        let objects = repo.common_dir().join("objects");
        let mut super_alternates = Vec::new();
        if let Ok(text) = std::fs::read_to_string(objects.join("info").join("alternates")) {
            for line in text.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
                let path = std::path::Path::new(line);
                super_alternates.push(if path.is_absolute() {
                    path.to_owned()
                } else {
                    objects.join(path)
                });
            }
        }
        AlternateSetup {
            location: string("submodule.alternateLocation"),
            error_strategy: string("submodule.alternateErrorStrategy"),
            super_alternates,
        }
    }
}

/// `prepare_possible_alternates()` (submodule--helper.c:1827-1863) plus the
/// `add_possible_reference_from_superproject()` it drives.
///
/// With `submodule.alternateLocation = superproject`, every alternate the
/// superproject borrows objects from is assumed to be a repository laid out the
/// same way, so `<that gitdir>/modules/<name>/` is where *its* copy of this
/// submodule would be — and if that is a usable repository it is added as a
/// `--reference` for the clone. `submodule.alternateErrorStrategy` decides what an
/// unusable one costs: `die` (the default) aborts the clone, `info` reports it and
/// carries on, `ignore` says nothing.
///
/// `Err` carries the message of a fatal, which the caller turns into exit code 128.
fn prepare_possible_alternates(
    setup: &AlternateSetup,
    sm_name: &BStr,
    references: &mut Vec<String>,
    out: &mut CloneOutput,
) -> std::result::Result<(), String> {
    let Some(location) = setup.location.as_deref() else {
        return Ok(());
    };
    let strategy = setup.error_strategy.as_deref().unwrap_or("die");
    if !matches!(strategy, "die" | "info" | "ignore") {
        return Err(format!(
            "fatal: Value '{strategy}' for submodule.alternateErrorStrategy is not recognized\n"
        ));
    }
    match location {
        "no" => return Ok(()),
        "superproject" => {}
        other => {
            return Err(format!(
                "fatal: Value '{other}' for submodule.alternateLocation is not recognized\n"
            ))
        }
    }

    for alternate in &setup.super_alternates {
        // "If the alternate object store is another repository, try the standard
        // layout with .git/(modules/<name>)+/objects".
        let Some(gitdir) = alternate.parent().filter(|_| alternate.file_name() == Some("objects".as_ref())) else {
            continue;
        };
        // The trailing `/` is git's: "We need to end the new path with '/' to mark
        // it as a dir, otherwise a submodule name containing '/' will be broken as
        // the last part of a missing submodule reference would be taken as a file
        // name." It is part of the string that reaches `--reference` and the
        // messages below, so it is built in rather than added at use.
        let candidate = format!(
            "{}/",
            gitdir
                .join("modules")
                .join(gix::path::from_bstr(sm_name).as_ref())
                .display()
        );
        match compute_alternate_path(std::path::Path::new(&candidate)) {
            Ok(()) => references.push(candidate),
            Err(err) => match strategy {
                "die" => {
                    return Err(format!(
                        "{ALTERNATE_ERROR_ADVICE}fatal: submodule '{sm_name}' cannot add alternate: {err}\n"
                    ))
                }
                "info" => out
                    .stderr
                    .extend_from_slice(format!("submodule '{sm_name}' cannot add alternate: {err}\n").as_bytes()),
                _ => {}
            },
        }
    }
    Ok(())
}

/// git's `alternate_error_advice`, printed by the `die` strategy before the fatal.
///
/// git gates it on `advice.submoduleAlternateErrorStrategyDie`, but the advice is
/// read inside the `submodule--helper clone` child, which does not see the
/// superproject's advice configuration — stock 2.55.0 prints these four lines even
/// with the key set to `false`, so they are unconditional here too.
const ALTERNATE_ERROR_ADVICE: &str = "hint: An alternate computed from a superproject's alternate is invalid.\n\
     hint: To allow Git to clone without an alternate in such a case, set\n\
     hint: submodule.alternateErrorStrategy to 'info' or, equivalently, clone with\n\
     hint: '--reference-if-able' instead of '--reference'.\n";

/// `compute_alternate_path()` (odb.c:274-338) reduced to its verdict: whether `path`
/// names a repository whose objects may be borrowed, and the message when it does not.
///
/// git returns the resolved gitdir; here only the four rejections matter, because
/// `add_possible_reference_from_superproject()` hands the *candidate* path — not the
/// resolved one — to `--reference`.
fn compute_alternate_path(path: &std::path::Path) -> std::result::Result<(), String> {
    let Ok(ref_git) = std::fs::canonicalize(path) else {
        return Err(format!("path '{}' does not exist", path.display()));
    };
    // `read_gitfile()`: a `.git` *file* redirects to the real repository.
    let redirect = read_gitfile(&ref_git).or_else(|| read_gitfile(&ref_git.join(".git")));
    let ref_git = match redirect {
        Some(repo) => repo,
        None if ref_git.join(".git").join("objects").is_dir() => ref_git.join(".git"),
        None if !ref_git.join("objects").is_dir() => {
            return Err(if ref_git.join("commondir").exists() {
                format!(
                    "reference repository '{}' as a linked checkout is not supported yet.",
                    path.display()
                )
            } else {
                format!("reference repository '{}' is not a local repository.", path.display())
            })
        }
        None => ref_git,
    };
    if ref_git.join("shallow").exists() {
        return Err(format!("reference repository '{}' is shallow", path.display()));
    }
    if ref_git.join("info").join("grafts").exists() {
        return Err(format!("reference repository '{}' is grafted", path.display()));
    }
    Ok(())
}

/// `read_gitfile()`: the path a `gitdir: <path>` file points at, resolved against
/// the file's own directory when it is relative.
fn read_gitfile(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let text = std::fs::read_to_string(path).ok()?;
    let target = text.strip_prefix("gitdir: ")?.trim_end();
    let target = std::path::Path::new(target);
    Some(if target.is_absolute() {
        target.to_owned()
    } else {
        path.parent()?.join(target)
    })
}

/// `is_empty_dir()`: a path that is not a directory, or is one with no entries.
/// A missing directory counts as empty, which is what git's `!stat(path)` guard
/// arranges by never reaching the check.
fn is_empty_dir(path: &std::path::Path) -> bool {
    match std::fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => true,
    }
}

/// `submodule_name_to_gitdir()` (submodule.c:2736-2768): `<git-dir>/modules/<name>`,
/// absolute. `modules` is on git's `common_list`, so every linked worktree of a
/// superproject shares one copy of each submodule.
///
/// The `extensions.submodulePathConfig` spelling — `submodule.<name>.gitdir`, plus
/// the containment check `validate_submodule_git_dir()` runs over the result — is
/// not ported, and is refused here rather than silently resolved to the plain path
/// it deliberately replaces.
fn submodule_name_to_gitdir(repo: &gix::Repository, name: &BStr) -> Result<std::path::PathBuf> {
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
    Ok(absolute(
        &repo
            .common_dir()
            .join("modules")
            .join(gix::path::from_bstr(name).as_ref()),
    ))
}

/// `absolute_pathdup()`: `path` made absolute without resolving symlinks, which is
/// what keeps `/tmp` from turning into `/private/tmp` in the paths git prints.
fn absolute(path: &std::path::Path) -> std::path::PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
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
    opts: &UpdateOpts,
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

    // `module_set_branch` hands `argv[0]` to `submodule_from_path` verbatim, and
    // `.gitmodules` records root-relative paths — so the operand is matched as
    // typed, never joined with the cwd prefix. Confirmed against git 2.55.0: run
    // from a subdirectory, `set-branch -b x sub` is the form that resolves and
    // `set-branch -b x ../sub` is the one that dies. (`set-url` behaves the same
    // way; only the *display* path in `sync`'s output is prefix-relative.)
    let Some(sub) = find_submodule(&submodules, &path) else {
        eprintln!("fatal: no submodule mapping found in .gitmodules for path '{path}'");
        return Ok(ExitCode::from(128));
    };
    let sub_name = sub.name().to_owned();
    let sub_name = sub_name.as_bstr();

    let modules_path = gitmodules_path(&repo);
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

/// git's `is_submodule_active` (submodule.c): `submodule.<name>.active` decides
/// on its own, otherwise the `submodule.active` pathspecs are matched against
/// the submodule's **path**, otherwise the submodule is active exactly when
/// `submodule.<name>.url` is set.
///
/// `gix::Submodule::is_active` is not used for the middle rule. It runs the
/// pathspecs against the submodule's *name* using a matcher of its own that
/// never sees the index, so a tree-wide `submodule.active = .` matches nothing
/// and every submodule comes back inactive: `status` then prints `-` for a
/// fully checked-out submodule and `--recursive` silently refuses to descend
/// into it. git matches the path through the index pathspec machinery, which is
/// what `repo.pathspec` reproduces here (`match_pathspec(..., is_dir = 1)`).
fn is_submodule_active(
    repo: &gix::Repository,
    index: &gix::index::State,
    sub: &gix::Submodule<'_>,
    path: &BString,
) -> Result<bool> {
    let name = sub.name();
    let snapshot = repo.config_snapshot();

    if let Some(active) = snapshot.boolean(key(name, "active")) {
        return Ok(active);
    }

    let specs: Vec<BString> = snapshot
        .plumbing()
        .strings("submodule.active")
        .unwrap_or_default();
    if !specs.is_empty() {
        let mut ps = repo.pathspec(
            false,
            &specs,
            false,
            index,
            gix::worktree::stack::state::attributes::Source::IdMapping,
        )?;
        return Ok(ps.is_included(path.as_bstr(), Some(true)));
    }

    Ok(snapshot.string(key(name, "url")).is_some())
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


/// A relative path from `from` to `to`, both absolute — enough for the
/// `gitdir:`/`core.worktree` pair git writes when it absorbs a submodule's git
/// directory. Falls back to the absolute target when the two share no prefix.
fn pathdiff_relative(from: &std::path::Path, to: &std::path::Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return to.iter().collect::<std::path::PathBuf>().to_string_lossy().into_owned();
    }
    let mut out = std::path::PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for c in &to[common..] {
        out.push(c);
    }
    out.to_string_lossy().into_owned()
}

/// `git submodule add [-b <branch>] [-f] [--name <name>] [--] <repository> [<path>]`
/// — clone `<repository>` into `<path>`, register it in `.gitmodules` and the
/// local config, and stage both the file and the new gitlink.
///
/// Ported behaviourally from git's `cmd_add`/`module_add` (builtin/submodule.c
/// plus git-submodule.sh's `add` in older versions): resolve the path, refuse a
/// path that is already occupied or already in the index, clone, write
/// `submodule.<name>.path` / `.url` into `.gitmodules`, mirror the url into the
/// repository config (git's `git config submodule.<name>.url`), then stage
/// `.gitmodules` and a mode-160000 entry at the clone's HEAD.
///
/// Built entirely out of already-ported pieces — the clone runs through this
/// binary's own `clone`, the gitlink through its own `update-index --cacheinfo`
/// — so there is no second implementation of either to drift.
///
/// Deliberate scope: `--reference`, `--depth`, `--ref-format` and the
/// `--branch`-tracking extras of newer git are not accepted; a relative
/// `<repository>` url is taken verbatim rather than resolved against the default
/// remote, the same restriction the rest of this port already documents.
fn add(args: &[String], quiet: bool) -> Result<ExitCode> {
    let mut force = false;
    let mut name: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut end_of_options = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_options || !a.starts_with('-') {
            rest.push(a.to_string());
            i += 1;
            continue;
        }
        let (opt, inline) = match a.split_once('=') {
            Some((o, v)) => (o, Some(v.to_string())),
            None => (a, None),
        };
        let mut value = |inline: Option<String>| -> Result<String> {
            match inline {
                Some(v) => Ok(v),
                None => {
                    i += 1;
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("option `{opt}' requires a value"))
                }
            }
        };
        match opt {
            "--" => end_of_options = true,
            "-f" | "--force" => force = true,
            "-q" | "--quiet" => {}
            "--name" => name = Some(value(inline)?),
            "-b" | "--branch" => branch = Some(value(inline)?),
            other => bail!("`submodule add {other}` is not ported"),
        }
        i += 1;
    }

    let url = match rest.first() {
        Some(u) => u.clone(),
        None => return usage_exit(),
    };
    // git defaults the path to the url's last component with a trailing `.git`
    // and any trailing slashes removed.
    let path = match rest.get(1) {
        Some(p) => p.trim_end_matches('/').to_string(),
        None => {
            let base = url.trim_end_matches('/').rsplit('/').next().unwrap_or(&url);
            base.strip_suffix(".git").unwrap_or(base).to_string()
        }
    };
    if path.is_empty() {
        crate::git_fatal!("'{url}' does not name a submodule path");
    }
    let name = name.unwrap_or_else(|| path.clone());

    let repo = gix::discover(".")?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("a working tree is required"))?
        .to_path_buf();

    // An occupied path is git's refusal, not something to clone over.
    let abs = workdir.join(&path);
    let occupied = std::fs::read_dir(&abs).map(|mut d| d.next().is_some()).unwrap_or(false);
    if occupied && !force {
        crate::git_fatal!("'{path}' already exists and is not an empty directory");
    }
    if !force && repo.index_or_empty()?.entry_by_path(BStr::new(path.as_bytes())).is_some() {
        crate::git_fatal!("'{path}' already exists in the index");
    }

    // ---- clone, through this binary's own porcelain -------------------------
    let mut clone_args = vec!["--".to_string(), url.clone(), path.clone()];
    if let Some(b) = &branch {
        clone_args.splice(0..0, ["--branch".to_string(), b.clone()]);
    }
    if quiet {
        clone_args.insert(0, "--quiet".to_string());
    }
    let code = super::clone(&clone_args)?;
    if code != ExitCode::SUCCESS {
        return Ok(code);
    }

    // ---- absorb the clone's git dir, as git does ----------------------------
    // `git submodule add` does not leave a standalone repository in the worktree:
    // the git dir moves to `<parent>/.git/modules/<name>` and the worktree gets a
    // `.git` FILE pointing at it, so the superproject owns the submodule's
    // objects and refs. Without this the tree looks right but every tool that
    // expects a gitfile (including this port's own crawler) sees a nested
    // independent repo.
    let modules_dir = repo.common_dir().join("modules").join(&name);
    if let Some(parent) = modules_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let cloned_git = abs.join(".git");
    if cloned_git.is_dir() && !modules_dir.exists() {
        std::fs::rename(&cloned_git, &modules_dir)?;
        // The gitfile is relative so the pair survives moving the checkout.
        let rel_to_modules = pathdiff_relative(&abs, &modules_dir);
        std::fs::write(&cloned_git, format!("gitdir: {rel_to_modules}\n"))?;
        // The moved git dir needs to know where its worktree went.
        let cfg_path = modules_dir.join("config");
        let mut cfg = ConfigFile::from_path_no_includes(cfg_path.clone(), Source::Local)?;
        let rel_to_worktree = pathdiff_relative(&modules_dir, &abs);
        cfg.set_raw_value_by("core", None, "worktree", rel_to_worktree.as_str())?;
        std::fs::write(&cfg_path, cfg.to_bstring())?;
    }

    // ---- register in .gitmodules and in the repository config ---------------
    let gitmodules = workdir.join(".gitmodules");
    let mut file = if gitmodules.exists() {
        ConfigFile::from_path_no_includes(gitmodules.clone(), Source::Worktree)?
    } else {
        ConfigFile::new(gix::config::file::Metadata::from(Source::Worktree).at(&gitmodules))
    };
    {
        let mut section = file.section_mut_or_create_new("submodule", Some(name.as_str().into()))?;
        section.push("path", Some(BStr::new(path.as_bytes())))?;
        section.push("url", Some(BStr::new(url.as_bytes())))?;
        if let Some(b) = &branch {
            section.push("branch", Some(BStr::new(b.as_bytes())))?;
        }
    }
    std::fs::write(&gitmodules, file.to_bstring())?;

    // git mirrors the url into the repository config so the submodule is
    // initialized in place, the same effect as a following `submodule init`.
    let local_config = repo.common_dir().join("config");
    let mut cfg = ConfigFile::from_path_no_includes(local_config.clone(), Source::Local)?;
    cfg.set_raw_value_by("submodule", Some(name.as_str().into()), "url", url.as_str())?;
    std::fs::write(&local_config, cfg.to_bstring())?;

    // ---- stage .gitmodules and the gitlink ---------------------------------
    let head = gix::open(&abs)?
        .head_id()
        .map_err(|_| anyhow::anyhow!("the cloned submodule has an unborn HEAD"))?
        .detach();
    let cacheinfo = format!("160000,{},{}", head.to_hex(), path);
    let staged = crate::dispatch::run(
        "update-index",
        &["--add".to_string(), "--cacheinfo".to_string(), cacheinfo],
    )?;
    if staged != ExitCode::SUCCESS {
        return Ok(staged);
    }
    let staged = crate::dispatch::run("add", &["--".to_string(), ".gitmodules".to_string()])?;
    if staged != ExitCode::SUCCESS {
        return Ok(staged);
    }

    if !quiet {
        println!("Adding existing repo at '{path}' to the index");
    }
    Ok(ExitCode::SUCCESS)
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

    /// Every `usage_with_options` block reached from this module, pinned to the
    /// bytes git 2.55.0 writes. The shape is load-bearing: `parse_options` ends
    /// each block with a blank line, and a block with a hidden-only option table
    /// (`absorbgitdirs`) is the usage line plus that blank line and nothing else.
    #[test]
    fn subcommand_usage_blocks_match_git() {
        assert_eq!(
            SET_URL_USAGE,
            "usage: git submodule set-url [--quiet] <path> <newurl>\n\n    \
             -q, --[no-]quiet      suppress output for setting url of a submodule\n\n"
        );
        assert_eq!(
            DEINIT_USAGE,
            "usage: git submodule deinit [--quiet] [-f | --force] [--all | [--] [<path>...]]\n\n    \
             -q, --[no-]quiet      suppress submodule status output\n    \
             -f, --[no-]force      remove submodule working trees even if they contain local changes\n    \
             --[no-]all            unregister all submodules\n\n"
        );
        assert_eq!(
            ABSORB_USAGE,
            "usage: git submodule absorbgitdirs [<options>] [<path>...]\n\n"
        );
        // The porcelain block is not a `parse_options` one: it comes from
        // `git-sh-setup`'s `$LONG_USAGE`, so it has no option table and no
        // trailing blank line, and `-h` prints it to stdout with status 0.
        assert!(USAGE.starts_with("usage: git submodule [--quiet] [--cached]\n"));
        assert!(USAGE.ends_with("absorbgitdirs [--] [<path>...]\n"));
        assert!(!USAGE.ends_with("\n\n"));
    }

    /// git's `sq_quote_buf` always wraps in single quotes, and escapes `'` and
    /// `!` by closing the quote, backslash-escaping the byte, and reopening —
    /// `!` included because a re-quoted string may be re-read by an interactive
    /// shell with history expansion on.
    #[test]
    fn sq_quote_matches_git() {
        let q = |s: &str| sq_quote(BStr::new(s.as_bytes()));
        assert_eq!(q("sub"), "'sub'");
        assert_eq!(q(""), "''");
        assert_eq!(q("with space"), "'with space'");
        assert_eq!(q("it's"), "'it'\\''s'");
        assert_eq!(q("bang!"), "'bang'\\!''");
        assert_eq!(q("a'b!c"), "'a'\\''b'\\!'c'");
    }

    /// git's `prepare_shell_cmd` only reaches for `sh` when the command word
    /// contains a shell metacharacter; otherwise the child is exec'd directly.
    /// The `foreach` single-argument form always gets `sh`, because the
    /// `path=…; ` prologue git prepends carries `;` and a space.
    #[test]
    fn shell_command_matches_prepare_shell_cmd() {
        let program = |argv: &[&str]| {
            let owned: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
            shell_command(&owned)
                .get_program()
                .to_string_lossy()
                .into_owned()
        };
        // No metacharacter in argv[0]: direct exec, even with several arguments.
        assert_eq!(program(&["true"]), "true");
        assert_eq!(program(&["git", "status", "--porcelain"]), "git");
        // Each of these argv[0]s carries a metacharacter from git's list.
        for word in ["echo hi", "a|b", "a$b", "a*b", "a=b", "a~b", "a#b", "a[b"] {
            assert_eq!(program(&[word]), "sh", "{word} should go through sh");
        }
    }

    /// `real_pathdup` has to answer for a path whose last component does not
    /// exist yet — `modules/<name>` before the relocation creates it — so the
    /// deepest existing ancestor is resolved and the rest appended.
    #[test]
    fn real_path_resolves_a_missing_leaf() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .expect("temp dir is resolvable");
        let missing = dir.join("zvcs-no-such-dir-9f3a2c").join("modules").join("x");
        assert_eq!(real_path(&missing), missing);
        assert_eq!(real_path(&dir), dir);
    }
}
