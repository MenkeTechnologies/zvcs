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
/// Four of the eleven stock subcommands are ported here, all aiming at
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
///     into the repository-local config, printing
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
/// `add`, `deinit`, `update`, `set-branch`, `set-url`, `sync` and
/// `absorbgitdirs`. `init` additionally bails on `.gitmodules` urls that are
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
        "update" => bail!("`submodule update` needs clone/fetch/checkout of submodules; not ported"),
        "deinit" => bail!("`submodule deinit` removes submodule worktrees; not ported"),
        "sync" => bail!("`submodule sync` rewrites remote urls inside submodules; not ported"),
        "set-branch" => bail!("`submodule set-branch` edits .gitmodules; not ported"),
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
    let index = repo.open_index()?;

    let mut entries = match module_list(&repo, &index, &patterns)? {
        Ok(entries) => entries,
        Err(code) => return Ok(ExitCode::from(code)),
    };

    let submodules = submodules(&repo)?;
    let prefix = repo_prefix(&repo)?;

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
            return Ok(ExitCode::from(128));
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
                return Ok(ExitCode::from(128));
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
    for line in messages {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
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
