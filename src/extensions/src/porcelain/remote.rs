use anyhow::Result;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::config::{File as ConfigFile, Source};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// `git remote` — manage the set of tracked repositories.
///
/// Implemented:
///   * `git remote [-v]`                       → names, optionally with URLs
///   * `git remote add [-f] [--tags|--no-tags] [-t <branch>]… [-m <master>]
///      [--mirror[=fetch|push]] <name> <url>`
///   * `git remote rename [--[no-]progress] <old> <new>`
///   * `git remote remove|rm <name>`
///   * `git remote set-head <name> (-d | <branch>)`
///   * `git remote set-branches [--add] <name> <branch>…`
///   * `git remote get-url [--push] [--all] <name>`
///   * `git remote set-url [--push] <name> <newurl> [<oldurl>]`,
///     `set-url --add`, `set-url --delete`
///   * `git remote show [-n] [<name>…]`        → detail block; without `-n`
///     the remote is contacted for the HEAD branch and per-branch status
///   * `git remote prune [-n] <name>`          → drop stale tracking refs
///   * `git remote update [-p] [<group>|<remote>]…`
///
/// Exit codes follow stock git: 0 success, 2 "no such remote", 3 "remote
/// already exists", 128 fatal, 129 usage error.
///
/// Known divergences:
///   * Online `show` reports the HEAD branch and the remote-branch
///     `new`/`tracked`/`stale` status from the ref advertisement, but the
///     `'git push'` section is still rendered in its `-n` (`(status not
///     queried)`) form: the per-ref push status (`up to date`,
///     `fast-forwardable`, `local out of date`, …) needs the remote objects in
///     the local object database to run the ahead/behind reachability check,
///     which a bare ref advertisement does not provide.
pub fn remote(args: &[String]) -> Result<ExitCode> {
    let mut verbose = false;
    let mut after_dashdash = false;
    let mut idx = 0;
    while let Some(orig) = args.get(idx).map(String::as_str) {
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`, which is why it is absent from `OPTS_MAIN` and checked
        // here instead. This table has no `PARSE_OPT_HIDDEN` entry, so
        // `USAGE_FULL` renders the same block `-h` prints.
        if orig == "--help-all" {
            return Ok(super::show_usage(&USAGE_MAIN));
        }
        let resolved = match canonical(orig, OPTS_MAIN, USAGE_MAIN) {
            Ok(name) => name,
            Err(code) => return Ok(code),
        };
        match resolved.as_ref() {
            "-v" | "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            "-h" | "--help" => {
                return Ok(super::show_usage(&USAGE_MAIN));
            }
            // `parse_options_step()` consumes a lone `--` before any table
            // lookup (parse-options.c: `if (!arg[2]) { ... ctx->argc--;
            // ctx->argv++; break; }`), so it ends option parsing and is never an
            // unknown option.
            "--" => {
                after_dashdash = true;
                idx += 1;
                break;
            }
            a if a.starts_with('-') => {
                unknown_option(a);
                return usage(USAGE_MAIN);
            }
            _ => break,
        }
        idx += 1;
    }
    let rest = &args[idx..];

    let mut repo = match crate::setup::discover() {
        Ok(repo) => repo,
        Err(_) => {
            return fatal("not a git repository (or any of the parent directories): .git");
        }
    };
    // `rename` and `set-head` write reflog lines; git signs those with
    // `git_committer_info(0)`, which falls back to the system identity
    // instead of refusing as a commit would.
    crate::ensure_reflog_identity(&mut repo);

    // The subcommand is only recognised by `parse_options()` ahead of a `--`
    // (`PARSE_OPT_SUBCOMMAND_OPTIONAL`); whatever follows one is a leftover argument, which
    // `cmd_remote()` answers with `unknown subcommand` even when it names a real one.
    if let (true, Some(other)) = (after_dashdash, rest.first()) {
        eprintln!("error: unknown subcommand: `{other}'");
        return usage(USAGE_MAIN);
    }

    match rest.first().map(String::as_str) {
        None => {
            // `show_all()` walks `for_each_remote()`, which runs remote.c's
            // `read_config()` first.
            crate::cmd_config::read_remote_config(&repo).map_err(|r| r.into_error())?;
            list(&repo, verbose)
        }
        Some("add") => add(&repo, &rest[1..]),
        Some("rename") => rename(&repo, &rest[1..]),
        Some("remove") | Some("rm") => remove(&repo, &rest[1..]),
        Some("set-head") => set_head(&repo, &rest[1..]),
        Some("set-branches") => set_branches(&repo, &rest[1..]),
        Some("get-url") => get_url(&repo, &rest[1..]),
        Some("set-url") => set_url(&repo, &rest[1..]),
        Some("show") => show(&repo, &rest[1..], verbose),
        Some("prune") => prune(&repo, &rest[1..]),
        Some("update") => update(&repo, &rest[1..], verbose),
        Some(other) => {
            eprintln!("error: unknown subcommand: `{other}'");
            usage(USAGE_MAIN)
        }
    }
}

// ---------------------------------------------------------------------------
// usage blocks (byte-identical to stock git's, which prints them on stderr)
// ---------------------------------------------------------------------------

const USAGE_MAIN: &str = "\
usage: git remote [-v | --verbose]
   or: git remote add [-t <branch>] [-m <master>] [-f] [--tags | --no-tags] [--mirror=<fetch|push>] <name> <url>
   or: git remote rename [--[no-]progress] <old> <new>
   or: git remote remove <name>
   or: git remote set-head <name> (-a | --auto | -d | --delete | <branch>)
   or: git remote [-v | --verbose] show [-n] <name>
   or: git remote prune [-n | --dry-run] <name>
   or: git remote [-v | --verbose] update [-p | --prune] [(<group> | <remote>)...]
   or: git remote set-branches [--add] <name> <branch>...
   or: git remote get-url [--push] [--all] <name>
   or: git remote set-url [--push] <name> <newurl> [<oldurl>]
   or: git remote set-url --add <name> <newurl>
   or: git remote set-url --delete <name> <url>

    -v, --[no-]verbose    be verbose; must be placed before a subcommand

";

const USAGE_ADD: &str = "\
usage: git remote add [<options>] <name> <url>

    -f, --[no-]fetch      fetch the remote branches
    --[no-]tags           import all tags and associated objects when fetching
                          or do not fetch any tag at all (--no-tags)
    -t, --[no-]track <branch>
                          branch(es) to track
    -m, --[no-]master <branch>
                          master branch
    --[no-]mirror[=(push|fetch)]
                          set up remote as a mirror to push to or fetch from

";

const USAGE_RENAME: &str = "\
usage: git remote rename [--[no-]progress] <old> <new>

    --[no-]progress       force progress reporting

";

const USAGE_REMOVE: &str = "\
usage: git remote remove <name>

";

const USAGE_SET_HEAD: &str = "\
usage: git remote set-head <name> (-a | --auto | -d | --delete | <branch>)

    -a, --[no-]auto       set refs/remotes/<name>/HEAD according to remote
    -d, --[no-]delete     delete refs/remotes/<name>/HEAD

";

const USAGE_SET_BRANCHES: &str = "\
usage: git remote set-branches <name> <branch>...
   or: git remote set-branches --add <name> <branch>...

    --[no-]add            add branch

";

const USAGE_GET_URL: &str = "\
usage: git remote get-url [--push] [--all] <name>

    --[no-]push           query push URLs rather than fetch URLs
    --[no-]all            return all URLs

";

const USAGE_SET_URL: &str = "\
usage: git remote set-url [--push] <name> <newurl> [<oldurl>]
   or: git remote set-url --add <name> <newurl>
   or: git remote set-url --delete <name> <url>

    --[no-]push           manipulate push URLs
    --[no-]add            add URL
    --[no-]delete         delete URLs

";

const USAGE_SHOW: &str = "\
usage: git remote show [<options>] <name>

    -n                    do not query remotes

";

const USAGE_PRUNE: &str = "\
usage: git remote prune [<options>] <name>

    -n, --[no-]dry-run    dry run

";

const USAGE_UPDATE: &str = "\
usage: git remote update [<options>] [<group> | <remote>]...

    -p, --[no-]prune      prune remotes after fetching

";

// ---------------------------------------------------------------------------
// option tables — one per `struct option options[]` in builtin/remote.c, so an
// unambiguous prefix resolves in each sub-command exactly as it does in stock
// git. `parse_long_opt()` skips `OPTION_SUBCOMMAND` entries (parse-options.c:
// 542-543) and entries with no `long_name`, which is why the top level lists
// only `verbose` and `show` lists nothing at all.
// ---------------------------------------------------------------------------

use super::{Arg, LongOpt};

/// `options[]` at builtin/remote.c:1939-1955: `OPT__VERBOSE` plus eleven
/// `OPT_SUBCOMMAND`s, none of which is a long option.
pub(super) const OPTS_MAIN: &[LongOpt] = &[LongOpt { name: "verbose", neg: true, arg: Arg::None }];

/// `add`'s table (builtin/remote.c:189-201).
pub(super) const OPTS_ADD: &[LongOpt] = &[
    LongOpt { name: "fetch", neg: true, arg: Arg::None },
    LongOpt { name: "tags", neg: true, arg: Arg::None },
    LongOpt { name: "track", neg: true, arg: Arg::Required },
    LongOpt { name: "master", neg: true, arg: Arg::Required },
    // `OPT_CALLBACK_F(... PARSE_OPT_OPTARG | PARSE_OPT_COMP_ARG ...)`.
    LongOpt { name: "mirror", neg: true, arg: Arg::Optional },
];

/// `rename`'s table (builtin/remote.c:860-862).
pub(super) const OPTS_RENAME: &[LongOpt] = &[LongOpt { name: "progress", neg: true, arg: Arg::None }];

/// `rm`/`remove`'s table (builtin/remote.c:1009-1011) is `OPT_END()` alone, so
/// no long option resolves and every `--x` is unknown.
const OPTS_REMOVE: &[LongOpt] = &[];

/// `show`'s table (builtin/remote.c:1425-1428) is a single
/// `OPT_BOOL('n', NULL, ...)`: a short-only entry, skipped by the long-option
/// resolver, so `git remote show --n` is unknown just like `--anything`.
const OPTS_SHOW: &[LongOpt] = &[];

/// `set-head`'s table (builtin/remote.c:1555-1560).
pub(super) const OPTS_SET_HEAD: &[LongOpt] = &[
    LongOpt { name: "auto", neg: true, arg: Arg::None },
    LongOpt { name: "delete", neg: true, arg: Arg::None },
];

/// `prune`'s table (builtin/remote.c:1673-1676): `OPT__DRY_RUN`, i.e.
/// `OPT_BOOL('n', "dry-run", ...)`.
pub(super) const OPTS_PRUNE: &[LongOpt] = &[LongOpt { name: "dry-run", neg: true, arg: Arg::None }];

/// `update`'s table (builtin/remote.c:1705-1708).
pub(super) const OPTS_UPDATE: &[LongOpt] = &[LongOpt { name: "prune", neg: true, arg: Arg::None }];

/// `set-branches`' table (builtin/remote.c:1788-1791).
pub(super) const OPTS_SET_BRANCHES: &[LongOpt] = &[LongOpt { name: "add", neg: true, arg: Arg::None }];

/// `get-url`'s table (builtin/remote.c:1811-1816).
pub(super) const OPTS_GET_URL: &[LongOpt] = &[
    LongOpt { name: "push", neg: true, arg: Arg::None },
    LongOpt { name: "all", neg: true, arg: Arg::None },
];

/// `set-url`'s table (builtin/remote.c:1856-1863).
pub(super) const OPTS_SET_URL: &[LongOpt] = &[
    LongOpt { name: "push", neg: true, arg: Arg::None },
    LongOpt { name: "add", neg: true, arg: Arg::None },
    LongOpt { name: "delete", neg: true, arg: Arg::None },
];

/// Respell one argument the way `parse_long_opt()` reads it, or hand back the
/// ambiguity refusal already rendered.
fn canonical<'a>(
    tok: &'a str,
    table: &'static [LongOpt],
    usage_text: &str,
) -> std::result::Result<std::borrow::Cow<'a, str>, ExitCode> {
    if let Some(code) = super::long_takes_no_value(tok, table) {
        return Err(code);
    }
    match super::canonical_long(tok, table) {
        super::Long::Name(name) => Ok(name),
        super::Long::Ambiguous(first, second) => {
            Err(super::ambiguous_option(tok, &first, &second, usage_text))
        }
    }
}

/// `parse_options()`' built-in help, asked of a *sub-command's* own parser.
///
/// Every `git remote` sub-command runs its own `parse_options()` over its own
/// `struct option options[]`, so once the sub-command word has been read `-h`
/// stops being `cmd_remote`'s question and becomes that sub-command's:
/// `git remote add -h` prints `builtin_remote_add_usage`, not
/// `builtin_remote_usage`. Both spellings reach
/// `usage_with_options_internal(..., USAGE_TO_STDOUT)`, so the block goes to
/// **stdout** with no `error:` line, at exit 0 — a refusal is on stderr at 129.
///
/// `shorts` names the sub-command's argument-less short options, which
/// [`super::asks_for_help`] needs to know where a cluster stops being understood;
/// none of `remote`'s tables has a `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` is
/// the block `-h` prints.
fn help_requested(tok: &str, shorts: &str, text: &str) -> Option<ExitCode> {
    super::asks_for_help(tok, shorts).then(|| super::show_usage(text))
}

/// Print a usage block and return git's usage exit code.
fn usage(text: &str) -> Result<ExitCode> {
    eprint!("{text}");
    Ok(ExitCode::from(129))
}

/// `error: <msg>` on stderr with an explicit exit code (2 = no such remote,
/// 3 = remote already exists).
fn error(msg: impl std::fmt::Display, code: u8) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    Ok(ExitCode::from(code))
}

/// `fatal: <msg>` on stderr, exit 128.
fn fatal(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// git distinguishes a long option (`--bogus`) from a short one (`-x`) in the
/// wording of the rejection; reproduce both.
fn unknown_option(arg: &str) {
    if let Some(long) = arg.strip_prefix("--") {
        eprintln!("error: unknown option `{long}'");
    } else {
        let short: String = arg.chars().skip(1).take(1).collect();
        eprintln!("error: unknown switch `{short}'");
    }
}

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

/// True when `remote.<name>.*` exists in any readable config scope.
/// The ref-store half of `read_config()` (remote.c:630-650), which every
/// `remote_get()`, `remote_is_configured()` and `for_each_remote()` runs before
/// it reads a single `remote.*` key:
///
/// ```c
/// if (startup_info->have_repository && !early) {
///         const char *head_ref = refs_resolve_ref_unsafe(
///                 get_main_ref_store(repo), "HEAD", 0, NULL, &flag);
/// ```
///
/// That lookup is the command's first use of the ref store, so a setting
/// `ref_store_init()` refuses (`-c core.logAllRefUpdates=none`) dies here, before
/// any remote is listed, looked up or written. The remote config itself is read
/// through gitoxide by the callers.
fn read_config(repo: &gix::Repository) {
    get_main_ref_store(repo);
}

/// `get_main_ref_store(the_repository)`: building the store is where
/// [`crate::setup`]'s first-use refusal fires, and looking up `HEAD` is the
/// cheapest store entry gitoxide offers.
fn get_main_ref_store(repo: &gix::Repository) {
    let _ = repo.find_reference("HEAD");
}

/// `remote_is_configured(remote, 1)` — every `git remote` subcommand's
/// existence test (builtin/remote.c:219, :883, :892, :1031, :1769, :1827,
/// :1884), and `in_repo` is not decoration:
///
/// ```c
/// int remote_is_configured(struct remote *remote, int in_repo)
/// {
///         if (!remote)
///                 return 0;
///         if (in_repo)
///                 return remote->configured_in_repo;
///         return !!remote->origin;
/// }
/// ```
///
/// `configured_in_repo` is set only while reading a `remote.<name>.*` key whose
/// scope is `CONFIG_SCOPE_LOCAL` or `CONFIG_SCOPE_WORKTREE` (remote.c:502-504),
/// so a key in `~/.gitconfig` names no remote here. That is why
/// `git config --global remote.upstream.prune true` does not make
/// `git remote rename origin upstream` a collision — the port answered from the
/// merged configuration and refused the rename with `remote upstream already
/// exists.`
/// [`remote_exists`], or defined by a legacy `remotes/` or `branches/` file:
/// `read_remotes_file()` and `read_branches_file()` set `configured_in_repo`
/// too (remote.c:360, 399).
fn remote_exists_or_legacy(repo: &gix::Repository, name: &str) -> bool {
    remote_exists(repo, name) || legacy(repo, name).is_some()
}

/// The legacy file definition of `name`, consulted only when the configuration
/// names no URL for it (remote.c:808-815).
pub(crate) fn legacy(repo: &gix::Repository, name: &str) -> Option<gix::remote::legacy::Remote> {
    if !effective_urls(repo, name, "url").is_empty() {
        return None;
    }
    crate::remote_legacy::lookup(repo, BStr::new(name))
}

fn remote_exists(repo: &gix::Repository, name: &str) -> bool {
    let cfg = repo.config_snapshot();
    let found = cfg.plumbing().sections().any(|section| {
        section.header().name() == "remote"
            && matches!(section.meta().source, Source::Local | Source::Worktree)
            && section.header().subsection_name().map(|s| s.to_str_lossy().into_owned())
                == Some(name.to_string())
    });
    found
}

/// git accepts a remote name exactly when it can stand in the destination of
/// the default fetch refspec, i.e. when `refs/remotes/<name>/test` is a valid
/// reference name.
fn valid_remote_name(name: &str) -> bool {
    let probe = format!("refs/remotes/{name}/test");
    gix::validate::reference::name(BStr::new(probe.as_bytes())).is_ok()
}

/// Effective values of `remote.<name>.<key>`, honouring git's rule that an
/// empty value clears everything configured before it.
fn effective_urls(repo: &gix::Repository, name: &str, key: &str) -> Vec<String> {
    let cfg = repo.config_snapshot();
    let Some(values) = cfg
        .plumbing()
        .strings_by("remote", Some(BStr::new(name)), key)
    else {
        return Vec::new();
    };

    let mut effective: Vec<BString> = Vec::new();
    for value in values {
        if value.is_empty() {
            effective.clear();
        } else {
            effective.push(value);
        }
    }
    effective
        .into_iter()
        .map(|u| u.to_str_lossy().into_owned())
        .collect()
}

/// The URLs `remote_get()` reads for `name` before any rewriting: the configured
/// ones, else a legacy file's, else the name itself (`add_url_alias()`,
/// remote.c:823-824).
fn raw_urls_or_name(repo: &gix::Repository, name: &str) -> Vec<String> {
    let urls = effective_urls(repo, name, "url");
    if urls.is_empty() {
        if let Some(legacy) = legacy(repo, name).filter(|l| !l.urls.is_empty()) {
            return legacy.urls.iter().map(|u| u.to_str_lossy().into_owned()).collect();
        }
        vec![name.to_string()]
    } else {
        urls
    }
}

/// `remote->url` and `push_url_of_remote(remote)` once `alias_all_urls()`
/// (remote.c:604-634) has run over `raw`: every URL and every configured push URL
/// through `url.<base>.insteadOf`; with no push URL configured, the push list is
/// each URL a `url.<base>.pushInsteadOf` rewrites (`add_pushurl_alias()`, :91-98),
/// and with none of those either it is the URL list (:959-962).
fn url_lists(repo: &gix::Repository, name: &str, raw: &[String]) -> (Vec<String>, Vec<String>) {
    use super::remote_http::{alias_url, alias_url_by};
    let snapshot = repo.config_snapshot();
    let file = snapshot.plumbing();
    let rewrite = |u: &String| alias_url(file, BStr::new(u)).to_str_lossy().into_owned();
    let configured_push = effective_urls(repo, name, "pushurl");
    let push: Vec<String> = match configured_push.is_empty() {
        false => configured_push.iter().map(rewrite).collect(),
        true => raw
            .iter()
            .filter_map(|u| alias_url_by(file, BStr::new(u), "pushInsteadOf"))
            .map(|u| u.to_str_lossy().into_owned())
            .collect(),
    };
    let urls: Vec<String> = raw.iter().map(rewrite).collect();
    let push = if push.is_empty() { urls.clone() } else { push };
    (urls, push)
}

/// The fetch URL list git would use, which falls back to the remote's own name
/// when nothing is configured.
fn fetch_urls_or_name(repo: &gix::Repository, name: &str) -> Vec<String> {
    url_lists(repo, name, &raw_urls_or_name(repo, name)).0
}

/// The push URL list: explicit `pushurl` entries, else the fetch URLs.
fn push_urls_or_name(repo: &gix::Repository, name: &str) -> Vec<String> {
    url_lists(repo, name, &raw_urls_or_name(repo, name)).1
}

/// The edit `repo_config_set_multivar(key, NULL, <pattern>, CONFIG_FLAGS_MULTI_REPLACE)`
/// makes to the repository's file (config.c:3028-3290): every value of
/// `remote.<name>.<key>` that `matches` accepts goes, line and all, where it stands,
/// and a section left with no entries goes with it (`maybe_remove_section()`,
/// :2823-2893). Returns how many values were removed — none is the caller's
/// `CONFIG_NOTHING_SET`.
fn unset_matching(file: &mut ConfigFile, name: &str, key: &str, matches: impl Fn(&[u8]) -> bool) -> usize {
    let ids: Vec<_> = file
        .sections_and_ids_by_name("remote")
        .map(|sections| {
            sections
                .filter(|(section, _)| section.header().subsection_name() == Some(BStr::new(name)))
                .map(|(_, id)| id)
                .collect()
        })
        .unwrap_or_default();
    let mut removed = 0;
    for id in ids {
        let emptied = {
            let Some(mut section) = file.section_mut_by_id(id) else {
                continue;
            };
            let hits: Vec<usize> = section
                .values(key)
                .iter()
                .enumerate()
                .filter(|(_, v)| matches(v))
                .map(|(i, _)| i)
                .collect();
            for &i in hits.iter().rev() {
                section.remove_at(key, i);
            }
            removed += hits.len();
            !hits.is_empty() && section.num_values() == 0
        };
        if emptied {
            file.remove_section_by_id(id);
        }
    }
    removed
}

/// Open the repository-local config for writing (`<common_dir>/config`).
fn open_local(repo: &gix::Repository) -> Result<(std::path::PathBuf, ConfigFile)> {
    let path = repo.common_dir().join("config");
    let file = ConfigFile::from_path_no_includes(path.clone(), Source::Local)?;
    Ok((path, file))
}

/// Serialize `file` to a sibling temp file and rename it over `path`, so a
/// crash never leaves a half-written config.
fn persist(path: &Path, file: &ConfigFile) -> Result<()> {
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

/// Subsection names of every `[branch "…"]` section in `file`, de-duplicated.
fn branch_subsections(file: &ConfigFile) -> Vec<BString> {
    let mut out: BTreeSet<BString> = BTreeSet::new();
    if let Some(sections) = file.sections_by_name("branch") {
        for section in sections {
            if let Some(sub) = section.header().subsection_name() {
                out.insert(sub.to_owned());
            }
        }
    }
    out.into_iter().collect()
}

/// Full names of every reference under `refs/remotes/<name>/`, with the target
/// each one carries.
///
/// **A ref whose file this repository cannot decode is still listed.** git's
/// every caller here goes through `refs_for_each_ref()`, whose iterator hands
/// the callback a broken loose ref with `REF_ISBROKEN` set rather than dropping
/// it — `for-each-ref` only skips those because `ref-filter.c` checks the flag
/// itself and says so (`warning: ignoring broken ref …`), while
/// `append_ref_to_tracked_list()` (builtin/remote.c) reads nothing but the
/// refname and therefore lists it. Measured against stock 2.55.0 on a
/// repository whose `extensions.objectFormat` says `sha256` while its loose
/// refs still hold 40-hex ids: `git for-each-ref` prints four
/// `warning: ignoring broken ref` lines and no rows, and `git remote show -n
/// origin` prints `div` and `main` under `Remote branches` and exits 0.
///
/// gitoxide's iterator reports the same file as a per-item
/// `ReferenceCreation` error instead, and propagating it aborted the whole
/// listing — `remote show -n` exited 1 having printed only the two URL lines.
/// The name is the one thing the error does carry, so it is recovered from
/// `relative_path` and the entry stands in with a null target, which is what
/// git's callback sees for a ref it could not parse.
fn tracking_refs(repo: &gix::Repository, name: &str) -> Result<Vec<(FullName, Target)>> {
    let prefix = format!("refs/remotes/{name}/");

    // The low-level store, not `repo.references()`: only its iterator yields the
    // typed `file::iter::Error` that names the ref it failed on. The high-level
    // one boxes the error into `dyn Error`, where the path is reachable only by
    // parsing the rendered message.
    let mut out = Vec::new();
    for reference in repo.refs.iter()?.prefixed(prefix.as_bytes().try_into()?)? {
        match reference {
            Ok(reference) => out.push((reference.name, reference.target)),
            Err(gix::refs::file::iter::loose_then_packed::Error::ReferenceCreation { relative_path, .. }) => {
                let Ok(broken) = FullName::try_from(gix::path::into_bstr(relative_path).as_ref())
                else {
                    continue;
                };
                out.push((broken, Target::Object(repo.object_hash().null())));
            }
            Err(e) => anyhow::bail!("{e}"),
        }
    }
    out.sort_by(|a, b| a.0.as_bstr().cmp(b.0.as_bstr()));
    Ok(out)
}

/// Parse a full name from a string that this module built, so the conversion
/// failing is a bug rather than user input.
fn full_name(name: &str) -> Result<FullName> {
    name.try_into()
        .map_err(|e| anyhow::anyhow!("invalid reference name '{name}': {e}"))
}

/// Delete `name`, reflog included.
fn delete_ref(repo: &gix::Repository, name: FullName) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Delete {
            expected: PreviousValue::Any,
            log: RefLog::AndReference,
            message: Default::default(),
        },
        name,
        deref: false,
    })?;
    Ok(())
}

/// True when the refspec destination `pattern` selects the reference `name`.
/// Only git's single-`*` glob form has to be handled here.
fn refspec_dst_matches(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, tail)) => {
            name.len() >= head.len() + tail.len() && name.starts_with(head) && name.ends_with(tail)
        }
    }
}

/// Destination halves of the configured fetch refspecs for `<name>`.
fn fetch_refspec_dsts(repo: &gix::Repository, name: &str) -> Vec<String> {
    effective_specs(repo, name, "fetch")
        .into_iter()
        .filter_map(|spec| {
            let body = spec.strip_prefix('+').unwrap_or(spec.as_str());
            body.split_once(':').map(|(_, dst)| dst.to_string())
        })
        .filter(|dst| !dst.is_empty())
        .collect()
}

/// Every ref name `refs_for_each_ref()` would visit, sorted, including one whose
/// file cannot be parsed (see [`tracking_refs`] for why that one is recovered
/// from the iterator's error).
fn all_ref_names(repo: &gix::Repository) -> Result<Vec<FullName>> {
    let mut out = Vec::new();
    for reference in repo.refs.iter()?.all()? {
        match reference {
            Ok(reference) => out.push(reference.name),
            Err(gix::refs::file::iter::loose_then_packed::Error::ReferenceCreation { relative_path, .. }) => {
                if let Ok(broken) = FullName::try_from(gix::path::into_bstr(relative_path).as_ref()) {
                    out.push(broken);
                }
            }
            Err(e) => anyhow::bail!("{e}"),
        }
    }
    out.sort_by(|a, b| a.as_bstr().cmp(b.as_bstr()));
    Ok(out)
}

/// `match_refname_with_pattern()` (refspec.c:298-324): whether `refname` matches
/// the single-`*` `pattern`, and with a `replacement` the name that `*` expands
/// to there.
fn match_refname_with_pattern(pattern: &[u8], refname: &[u8], replacement: Option<&[u8]>) -> Option<Vec<u8>> {
    let kstar = pattern.find_byte(b'*')?;
    let (head, tail) = (&pattern[..kstar], &pattern[kstar + 1..]);
    if !(refname.starts_with(head) && refname.len() >= head.len() + tail.len() && refname.ends_with(tail)) {
        return None;
    }
    let middle = &refname[head.len()..refname.len() - tail.len()];
    Some(match replacement.and_then(|r| r.find_byte(b'*').map(|v| (r, v))) {
        Some((r, vstar)) => [&r[..vstar], middle, &r[vstar + 1..]].concat(),
        None => Vec::new(),
    })
}

/// `remote_find_tracking(remote, &query)` with `query.dst = dst` — true where git
/// returns 0: `refspec_find_match()` (refspec.c:434-466) over the remote's fetch
/// refspecs, after `refspec_find_negative_match()` (refspec.c:346-400) has had its
/// say. A `^` refspec is matched against the *source* the destination maps back
/// to, which is why the reversal expands every positive refspec first.
fn remote_find_tracking(repo: &gix::Repository, remote: &str, dst: &BStr) -> bool {
    tracking_source(repo, remote, dst).is_some()
}

/// [`remote_find_tracking`]'s `query.src`: the remote ref the first matching
/// refspec maps `dst` back from (`query_matches_negative_refspec()` and
/// `refspec_find_match()`, refspec.c:346-466).
fn tracking_source(repo: &gix::Repository, remote: &str, dst: &BStr) -> Option<Vec<u8>> {
    use gix::refspec::instruction::Fetch;
    use gix::refspec::Instruction;
    let Ok(remote) = repo.find_remote(remote) else {
        return None;
    };
    let needle: &[u8] = dst.as_ref();
    let is_pattern = |s: &[u8]| s.contains(&b'*');
    let specs: Vec<Instruction<'_>> = remote
        .refspecs(gix::remote::Direction::Fetch)
        .iter()
        .map(|s| s.to_ref().instruction())
        .collect();

    // The first loop of `refspec_find_negative_match()`: every positive refspec,
    // read backwards. Note the non-pattern arm compares the needle with the
    // *source*, as the C does.
    let mut reversed: Vec<Vec<u8>> = Vec::new();
    for spec in &specs {
        let (src, key): (&[u8], &[u8]) = match spec {
            Instruction::Fetch(Fetch::Only { src }) => (src.as_ref(), src.as_ref()),
            Instruction::Fetch(Fetch::AndUpdate { src, dst, .. }) => (src.as_ref(), dst.as_ref()),
            _ => continue,
        };
        if is_pattern(key) {
            if let Some(expanded) = match_refname_with_pattern(key, needle, Some(src)) {
                reversed.push(expanded);
            }
        } else if needle == src {
            reversed.push(src.to_vec());
        }
    }
    // `refname_matches_negative_refspec_item()` (refspec.c:335-344).
    let negative = |name: &[u8]| {
        specs.iter().any(|spec| match spec {
            Instruction::Fetch(Fetch::Exclude { src }) => {
                let src: &[u8] = src.as_ref();
                match is_pattern(src) {
                    true => match_refname_with_pattern(src, name, None).is_some(),
                    false => src == name,
                }
            }
            _ => false,
        })
    };
    if reversed.iter().any(|name| negative(name)) {
        return None;
    }

    specs.iter().find_map(|spec| {
        let Instruction::Fetch(Fetch::AndUpdate { src, dst, .. }) = spec else {
            return None;
        };
        let (src, dst): (&[u8], &[u8]) = (src.as_ref(), dst.as_ref());
        match is_pattern(dst) {
            true => match_refname_with_pattern(dst, needle, Some(src)),
            false => (dst == needle).then(|| src.to_vec()),
        }
    })
}

/// The `fetch` or `push` refspecs of `<name>`: the configured ones, then those
/// of a legacy file (`refspec_append()` in `read_remotes_file()` and
/// `read_branches_file()` adds to the configured list).
fn effective_specs(repo: &gix::Repository, name: &str, key: &str) -> Vec<String> {
    let mut specs = configured_specs(repo, name, key);
    if let Some(legacy) = legacy(repo, name) {
        let extra = if key == "push" { &legacy.push } else { &legacy.fetch };
        specs.extend(extra.iter().map(|s| s.to_str_lossy().into_owned()));
    }
    specs
}

/// Raw multi-values of `remote.<name>.<key>` across all config scopes.
fn configured_specs(repo: &gix::Repository, name: &str, key: &str) -> Vec<String> {
    let cfg = repo.config_snapshot();
    let values: Vec<String> = cfg
        .plumbing()
        .strings_by("remote", Some(BStr::new(name)), key)
        .map(|values| {
            values
                .into_iter()
                .map(|v| v.to_str_lossy().into_owned())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();

    // A refspec is a multivar, so this list is one entry per *configured* value —
    // and every valued `-c` reaches the snapshot twice, once as `Source::Cli` and
    // once through the `GIT_CONFIG_COUNT` environment channel children inherit
    // (see [`crate::setup::double_delivered`]). Resolution is unaffected, but a
    // count is: `git -c remote.origin.push=a:b remote show -n origin` listed the
    // one refspec twice and headed the block with the plural `Local refs`. One
    // copy per recorded override is discounted here, so a value that is *also*
    // in the config file still contributes its own entry.
    // The section and the value name are matched case-insensitively and the
    // subsection exactly, which is how `git_config_parse_key` canonicalizes a key.
    let mut discount: std::collections::HashMap<&str, usize> = Default::default();
    for (k, v) in crate::setup::double_delivered() {
        let Some((section, rest)) = k.split_once('.') else {
            continue;
        };
        let Some((subsection, value_name)) = rest.rsplit_once('.') else {
            continue;
        };
        if section.eq_ignore_ascii_case("remote")
            && subsection == name
            && value_name.eq_ignore_ascii_case(key)
        {
            *discount.entry(v.trim()).or_default() += 1;
        }
    }
    if discount.is_empty() {
        return values;
    }
    values
        .into_iter()
        .filter(|value| match discount.get_mut(value.trim()) {
            Some(count) if *count > 0 => {
                *count -= 1;
                false
            }
            _ => true,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// remote [-v]
// ---------------------------------------------------------------------------

/// Print configured remote names, optionally with their fetch/push URLs.
///
/// A remote without any URL prints just `<name>\t` in verbose mode — git lists
/// it from the config section without applying the name-as-URL fallback that
/// `show` and `get-url` use.
fn list(repo: &gix::Repository, verbose: bool) -> Result<ExitCode> {
    // `show_all()` → `for_each_remote()` (builtin/remote.c:1401, remote.c:865-868).
    read_config(repo);
    // `get_one_entry()` (builtin/remote.c:1366-1394) builds every entry before
    // `show_all()` prints any, reading `remote.<name>.partialclonefilter` with
    // `repo_config_get_string_tmp()` for a remote with a URL — so a valueless
    // filter dies through `git_die_config()` before anything is printed, and a
    // valued one is appended to the fetch line as ` [<filter>]`.
    // The entries are built with or without `-v`; only the printing differs.
    let mut lines: Vec<String> = Vec::new();
    for name in repo.remote_names() {
        let name = name.to_str_lossy();
        let (fetch, push) = url_lists(repo, &name, &effective_urls(repo, &name, "url"));
        let filter = fetch.first().and_then(|_| {
            crate::config::config_get_string(Some(repo), &format!("remote.{name}.partialclonefilter"))
        });
        if !verbose {
            lines.push(name.into_owned());
            continue;
        }
        match fetch.first() {
            Some(url) => {
                let filter = filter.map(|f| format!(" [{f}]")).unwrap_or_default();
                lines.push(format!("{name}\t{url} (fetch){filter}"));
            }
            None => lines.push(format!("{name}\t")),
        }
        // `url = push_url_of_remote(remote);` and a line for each of them
        // (builtin/remote.c:1386-1391).
        for url in &push {
            lines.push(format!("{name}\t{url} (push)"));
        }
    }
    for line in lines {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// remote add
// ---------------------------------------------------------------------------

/// What `--mirror` was asked for. A bare `--mirror` means both halves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mirror {
    No,
    Fetch,
    Push,
    Both,
}

impl Mirror {
    fn fetches(self) -> bool {
        matches!(self, Mirror::Fetch | Mirror::Both)
    }
    fn pushes(self) -> bool {
        matches!(self, Mirror::Push | Mirror::Both)
    }
}

/// `git remote add` — write `remote.<name>.{url,fetch,tagOpt,mirror}` and,
/// with `-m`, the `refs/remotes/<name>/HEAD` symref.
fn add(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut do_fetch = false;
    let mut tag_opt: Option<&str> = None;
    let mut tracks: Vec<String> = Vec::new();
    let mut master: Option<String> = None;
    let mut mirror = Mirror::No;
    let mut warn_mirror = false;
    let mut pos: Vec<&str> = Vec::new();

    let mut i = 0;
    while let Some(orig) = args.get(i).map(String::as_str) {
        i += 1;
        if let Some(code) = help_requested(orig, "f", USAGE_ADD) {
            return Ok(code);
        }
        let resolved = match canonical(orig, OPTS_ADD, USAGE_ADD) {
            Ok(name) => name,
            Err(code) => return Ok(code),
        };
        let a = resolved.as_ref();
        match a {
            "-f" | "--fetch" => do_fetch = true,
            "--no-fetch" => do_fetch = false,
            "--tags" => tag_opt = Some("--tags"),
            "--no-tags" => tag_opt = Some("--no-tags"),
            "--mirror" => {
                mirror = Mirror::Both;
                warn_mirror = true;
            }
            "--mirror=fetch" => mirror = Mirror::Fetch,
            "--mirror=push" => mirror = Mirror::Push,
            "--no-mirror" => mirror = Mirror::No,
            "--no-track" => tracks.clear(),
            "--no-master" => master = None,
            "-t" | "--track" => match args.get(i) {
                Some(v) => {
                    tracks.push(v.clone());
                    i += 1;
                }
                None => return usage(USAGE_ADD),
            },
            "-m" | "--master" => match args.get(i) {
                Some(v) => {
                    master = Some(v.clone());
                    i += 1;
                }
                None => return usage(USAGE_ADD),
            },
            // `parse_mirror_opt()` returns `error()`, which `parse_options()` turns
            // into exit 129 with no usage block (builtin/remote.c:144-159,
            // parse-options.c:1210-1211).
            _ if a.starts_with("--mirror=") => {
                eprintln!(
                    "error: unknown --mirror argument: {}",
                    &a["--mirror=".len()..]
                );
                return Ok(ExitCode::from(129));
            }
            _ if a.starts_with("--track=") => tracks.push(a["--track=".len()..].to_string()),
            _ if a.starts_with("--master=") => master = Some(a["--master=".len()..].to_string()),
            _ if a.starts_with("-t") && a.len() > 2 => tracks.push(a[2..].to_string()),
            _ if a.starts_with("-m") && a.len() > 2 => master = Some(a[2..].to_string()),
            _ if a.starts_with('-') && a.len() > 1 => {
                unknown_option(a);
                return usage(USAGE_ADD);
            }
            _ => pos.push(orig),
        }
    }

    if warn_mirror {
        eprintln!(
            "warning: --mirror is dangerous and deprecated; please\n\t use --mirror=fetch or --mirror=push instead"
        );
    }
    if pos.len() != 2 {
        return usage(USAGE_ADD);
    }
    let (name, url) = (pos[0], pos[1]);

    // builtin/remote.c:213-225 in order: the mirror refusal, `remote_get()`
    // (whose `read_config()` is the ref store's first use), the collision, and
    // only then the name check.
    if mirror != Mirror::No && master.is_some() {
        return fatal("specifying a master branch makes no sense with --mirror");
    }
    if !tracks.is_empty() && mirror == Mirror::Push {
        return fatal("specifying branches to track makes sense only with fetch mirrors");
    }
    read_config(repo);
    if remote_exists_or_legacy(repo, name) {
        return error(format!("remote {name} already exists."), 3);
    }
    if !valid_remote_name(name) {
        return fatal(format!("'{name}' is not a valid remote name"));
    }
    // ```c
    // static int check_remote_collision(struct remote *remote, void *data)
    // {
    //         const char *name = data;
    //         const char *p;
    //
    //         if (skip_prefix(name, remote->name, &p) && *p == '/')
    //                 die(_("remote name '%s' is a subset of existing remote '%s'"),
    //                     name, remote->name);
    //         if (skip_prefix(remote->name, name, &p) && *p == '/')
    //                 die(_("remote name '%s' is a superset of existing remote '%s'"),
    //                     name, remote->name);
    //
    //         return 0;
    // }
    // …
    // for_each_remote(check_remote_collision, (void *)name);
    // ```
    //
    // (builtin/remote.c:162-175, :227.) `outer` and `outer/inner` would both
    // want `refs/remotes/outer/…`, and one of them would end up owning the
    // other's tracking refs — so the second is refused before anything is
    // written. This was absent, and `git remote add outer/inner url` succeeded
    // next to an existing `outer`.
    for existing in repo.remote_names() {
        let existing = existing.to_str_lossy();
        if name.strip_prefix(existing.as_ref()).is_some_and(|p| p.starts_with('/')) {
            return fatal(format!(
                "remote name '{name}' is a subset of existing remote '{existing}'"
            ));
        }
        if existing.strip_prefix(name).is_some_and(|p| p.starts_with('/')) {
            return fatal(format!(
                "remote name '{name}' is a superset of existing remote '{existing}'"
            ));
        }
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let (path, mut file) = open_local(repo)?;
    {
        let mut section = file.section_mut_or_create_new("remote", Some(BStr::new(name)))?;
        section.push("url", url)?;
        // ```c
        // if (!mirror || mirror & MIRROR_FETCH) {
        //         [...]
        //         for (size_t i = 0; i < track.nr; i++)
        //                 add_branch(buf.buf, track.items[i].string, name, mirror, &buf2);
        // }
        // ```
        //
        // (`add()`, builtin/remote.c:232-241.) A push-only mirror gets no fetch refspec at
        // all — the remote exists to be pushed to, and `git fetch up` on it fetches nothing
        // rather than the whole branch namespace.
        //
        // `add_branch()` (builtin/remote.c:126-138) maps each tracked branch — `*` when none
        // is named — to `+refs/<branch>:refs/<branch>` for a mirror and to
        // `+refs/heads/<branch>:refs/remotes/<name>/<branch>` otherwise.
        if mirror == Mirror::No || mirror.fetches() {
            let all = ["*".to_owned()];
            let branches = if tracks.is_empty() { &all[..] } else { &tracks[..] };
            for branch in branches {
                let spec = match mirror {
                    Mirror::No => format!("+refs/heads/{branch}:refs/remotes/{name}/{branch}"),
                    _ => format!("+refs/{branch}:refs/{branch}"),
                };
                section.push("fetch", spec.as_str())?;
            }
        }
        // `remote.<name>.mirror` is written before `remote.<name>.tagOpt`
        // (builtin/remote.c:243-254).
        if mirror.pushes() {
            section.push("mirror", "true")?;
        }
        if let Some(t) = tag_opt {
            section.push("tagOpt", t)?;
        }
    }
    persist(&path, &file)?;

    if do_fetch {
        // `Updating %s` is `printf`, not `fprintf(stderr, …)`
        // (builtin/remote.c's `add` arm), like the `Fetching %s` that
        // `remote update` prints from the same file.
        println!("Updating {name}");
        // git branches on `cmd_fetch()`'s int: anything but 0 is
        // `error: Could not fetch <name>` and exit 1.
        //
        // `fetch()` reports a failed fetch as `Ok(ExitCode::from(128))`, not as
        // `Err` — `Err` is reserved for the paths that die. Testing only
        // `is_err()` therefore missed every ordinary failure, so
        // `git remote add -f origin <missing repo>` printed the transport's
        // `ERROR: Repository not found.` and then exited **0**, telling a script
        // that checks `$?` the fetch had succeeded.
        let failed = match super::fetch::fetch(&[name.to_string()]) {
            Ok(code) => code != ExitCode::SUCCESS,
            Err(_) => true,
        };
        if failed {
            eprintln!("error: Could not fetch {name}");
            return Ok(ExitCode::from(1));
        }
    }

    // ```c
    // if (refs_update_symref(get_main_ref_store(the_repository), buf.buf, buf2.buf, "remote add"))
    //         result = error(_("Could not setup master '%s'"), master);
    // ```
    //
    // (`add()`, builtin/remote.c:261-270.) It runs *after* the `-f` fetch, so the branch the
    // symref names is already there and the reflog line records the object it resolves to.
    if let Some(branch) = &master {
        let head = format!("refs/remotes/{name}/HEAD");
        let target = format!("refs/remotes/{name}/{branch}");
        let previous = resolved_id(repo, head.as_str())?;
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "remote add".into(),
                },
                expected: PreviousValue::Any,
                new: Target::Symbolic(full_name(&target)?),
            },
            name: full_name(&head)?,
            deref: false,
        })?;
        log_symref_update(repo, &head, previous, "remote add")?;
    }

    Ok(ExitCode::SUCCESS)
}

/// `refs_update_symref()` appends a reflog line for the symbolic ref itself, holding the
/// object its new target resolves to — which is how `refs/remotes/<name>/HEAD` ends up with
/// a `remote set-head` / `remote add` entry. gitoxide writes no log at all for a symbolic
/// edit, so the line is appended here, after the edit that resolved the target.
///
/// `previous` is what the ref resolved to *before* the edit, so the caller reads it first.
pub(super) fn log_symref_update(
    repo: &gix::Repository,
    name: &str,
    previous: Option<gix::ObjectId>,
    message: &str,
) -> Result<()> {
    // The reftable backend logs a symref update in the same transaction, as
    // `write_transaction_table()` (refs/reftable-backend.c:1540-1599) does.
    if crate::refstore::is_reftable(repo) {
        return Ok(());
    }
    let full = full_name(name)?;
    let Some(new) = resolved_id(repo, name)? else {
        return Ok(());
    };
    super::symbolic_ref::append_reflog(repo, full.as_ref().as_bstr(), previous, &new, message)
}

/// The object a ref resolves to, or `None` when it does not exist or does not peel.
pub(super) fn resolved_id(repo: &gix::Repository, name: &str) -> Result<Option<gix::ObjectId>> {
    let Some(mut reference) = repo.try_find_reference(name)? else {
        return Ok(None);
    };
    Ok(reference.peel_to_id_in_place().ok().map(|id| id.detach()))
}

// ---------------------------------------------------------------------------
// remote rename
// ---------------------------------------------------------------------------

/// `migrate_file()` (builtin/remote.c:762-788): write a legacy remote's URLs,
/// push and fetch refspecs into `remote.<name>.*`, in that order, then remove
/// the file it came from — `remotes/<name>` when that is what defined it,
/// `branches/<name>` otherwise.
fn migrate_file(repo: &gix::Repository, name: &str, legacy: &gix::remote::legacy::Remote) -> Result<ExitCode> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let (path, mut file) = open_local(repo)?;
    {
        let mut section = file.section_mut_or_create_new("remote", Some(BStr::new(name)))?;
        for (key, values) in [("url", &legacy.urls), ("push", &legacy.push), ("fetch", &legacy.fetch)] {
            for value in values {
                section.push(key, value.as_bstr())?;
            }
        }
    }
    persist(&path, &file)?;
    let remotes = repo.common_dir().join("remotes").join(name);
    let origin = if remotes.is_file() {
        remotes
    } else {
        repo.common_dir().join("branches").join(name)
    };
    // `unlink_or_warn()`.
    if let Err(e) = std::fs::remove_file(&origin) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "warning: unable to unlink '{}': {}",
                origin.display(),
                crate::external::strerror(&e)
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `git remote rename <old> <new>` — move the config section, the tracking
/// refs and every `branch.*` / `remote.pushDefault` back-reference.
///
/// The ordering mirrors stock git: config first, then the refs, then the fetch
/// refspecs. A ref collision therefore aborts with the section already renamed
/// and the refspecs untouched, exactly as git leaves it.
fn rename(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut pos: Vec<&str> = Vec::new();
    for orig in args {
        if let Some(code) = help_requested(orig, "", USAGE_RENAME) {
            return Ok(code);
        }
        let resolved = match canonical(orig, OPTS_RENAME, USAGE_RENAME) {
            Ok(name) => name,
            Err(code) => return Ok(code),
        };
        match resolved.as_ref() {
            "--progress" | "--no-progress" => {}
            s if s.starts_with('-') && s.len() > 1 => {
                unknown_option(s);
                return usage(USAGE_RENAME);
            }
            _ => pos.push(orig.as_str()),
        }
    }
    if pos.len() != 2 {
        return usage(USAGE_RENAME);
    }
    let (old, new) = (pos[0], pos[1]);

    // `oldremote = remote_get(rename.old_name)` (builtin/remote.c:882).
    read_config(repo);
    // ```c
    // for (int i = 0; i < oldremote->fetch.nr && !refspecs_need_update; i++)
    //         refspecs_need_update = !!strstr(oldremote->fetch.items[i].raw,
    //                                         old_remote_context.buf);
    // ```
    //
    // (builtin/remote.c:910-912.) Only a remote whose fetch refspecs write into
    // `refs/remotes/<old>/` has its references there renamed and
    // `remote.pushDefault` followed (`handle_push_default()` sits in the same
    // `if (refspecs_need_update)`, :985-992); any other keeps both.
    let old_context = format!(":refs/remotes/{old}/");
    let refspecs_need_update = effective_specs(repo, old, "fetch")
        .iter()
        .any(|spec| spec.contains(&old_context));
    if !remote_exists_or_legacy(repo, old) {
        return error(format!("No such remote: '{old}'"), 2);
    }
    // ```c
    // if (!strcmp(rename.old_name, rename.new_name) && oldremote->origin != REMOTE_CONFIG)
    //         return migrate_file(oldremote);
    // ```
    //
    // (builtin/remote.c:888-889.) Renaming a legacy remote onto itself is the
    // migration its deprecation warning recommends.
    if old == new {
        if let Some(legacy) = legacy(repo, old) {
            return migrate_file(repo, old, &legacy);
        }
    }
    if !valid_remote_name(new) {
        return fatal(format!("'{new}' is not a valid remote name"));
    }
    if remote_exists(repo, new) {
        return error(format!("remote {new} already exists."), 3);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // 1. config: section header and every back-reference to the old name.
    let (path, mut file) = open_local(repo)?;
    // ```c
    // if (repo_config_rename_section(the_repository, buf.buf, buf2.buf) < 1) {
    //         result = error(_("Could not rename config section '%s' to '%s'"),
    //                        buf.buf, buf2.buf);
    //         goto out;
    // }
    // ```
    //
    // (builtin/remote.c:900-906.) A remote that exists without a section in the
    // repository's own configuration — one read from a legacy file — ends here.
    if file
        .rename_section("remote", Some(BStr::new(old)), "remote", new.to_string())
        .is_err()
    {
        return error(
            format!("Could not rename config section 'remote.{old}' to 'remote.{new}'"),
            1,
        );
    }

    for branch in branch_subsections(&file) {
        let Ok(mut section) = file.section_mut("branch", Some(branch.as_bstr())) else {
            continue;
        };
        for key in ["remote", "pushRemote"] {
            if section.value(key).map(|v| v == old).unwrap_or(false) {
                section.set(key, new)?;
            }
        }
    }
    if refspecs_need_update {
        handle_push_default(repo, &mut file, old, Some(new))?;
    }
    persist(&path, &file)?;

    // 2. refs: refuse before touching anything if a destination is occupied.
    //
    // A reftable store gets git 2.56's single transaction instead, which the
    // files emulation below is not (see [`rename_refs_in_one_transaction`]).
    let reftable = crate::refstore::is_reftable(repo);
    if reftable {
        if let Some(code) = rename_refs_in_one_transaction(repo, old, new)? {
            return Ok(code);
        }
    }
    let refs = match reftable || !refspecs_need_update {
        true => Vec::new(),
        false => tracking_refs(repo, old)?,
    };
    let mut moves = Vec::with_capacity(refs.len());
    for (name, target) in refs {
        let text = name.as_bstr().to_str_lossy().into_owned();
        let Some(suffix) = text.strip_prefix(&format!("refs/remotes/{old}/")) else {
            continue;
        };
        let dst = format!("refs/remotes/{new}/{suffix}");
        if repo.try_find_reference(dst.as_str())?.is_some() {
            return error(
                format!("renaming remote references failed: cannot lock ref '{dst}': reference already exists"),
                128,
            );
        }
        moves.push((name, text, dst, target));
    }

    // ```c
    // if (refs_rename_ref(get_main_ref_store(the_repository), item->string, buf.buf, buf2.buf))
    // ```
    //
    // (`mv()`, builtin/remote.c.) A rename keeps the ref's value, so the log line it appends
    // opens with that value rather than with the null id a freshly created ref would show.
    // gitoxide has no rename — the ref is created and the old one deleted — so its own
    // logging is switched off for the edit and the line is written by hand.
    //
    // "Switched off" is the *order* as much as the handle: the create runs while the
    // destination still has no log, so neither the `Disable` policy nor anything else
    // has a file to append to; the log is carried across immediately afterwards, and
    // only then is the single `<tip> <tip>` line written. Moving the logs up front
    // instead — as this used to — gives the create a file to append to and the rename
    // two entries where git writes one.
    let mut silent = repo.clone();
    silent.refs.write_reflog = gix::refs::store::WriteReflog::Disable;

    for (old_name, old_text, dst, target) in moves {
        let new_name = full_name(&dst)?;
        let new_target = match target {
            Target::Object(id) => Target::Object(id),
            Target::Symbolic(sym) => {
                let text = sym.as_bstr().to_str_lossy().into_owned();
                match text.strip_prefix(&format!("refs/remotes/{old}/")) {
                    Some(suffix) => {
                        Target::Symbolic(full_name(&format!("refs/remotes/{new}/{suffix}"))?)
                    }
                    None => Target::Symbolic(sym),
                }
            }
        };
        let message = format!("remote: renamed {old_text} to {dst}");
        let logged = match &new_target {
            Target::Object(id) => Some(*id),
            Target::Symbolic(_) => None,
        };
        silent.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: message.clone().into(),
                },
                expected: PreviousValue::Any,
                new: new_target,
            },
            name: new_name.clone(),
            deref: false,
        })?;
        // `rename_one_reflog()` (builtin/remote.c:672-711) copies the ref's reflog to
        // the new name and appends the `remote: renamed` entry — and does neither for a
        // ref that has no reflog (`if (!refs_reflog_exists(...)) return 0;`).
        // A symbolic ref is queued with a NULL `oid` (builtin/remote.c:724-736), so its entry is
        // null to null.
        let had_reflog = crate::refstore::reflog_exists(repo, &old_text);
        move_reflog(repo, &old_text, &dst)?;
        if had_reflog {
            let id = logged.unwrap_or_else(|| repo.object_hash().null());
            super::symbolic_ref::append_reflog(repo, new_name.as_ref().as_bstr(), Some(id), &id, &message)?;
        }
        delete_ref(repo, old_name)?;
    }

    // 3. fetch refspecs that point into the old tracking namespace.
    let (path, mut file) = open_local(repo)?;
    let context = format!(":refs/remotes/{old}/");
    let replacement = format!(":refs/remotes/{new}/");
    {
        let Ok(mut section) = file.section_mut("remote", Some(BStr::new(new))) else {
            return Ok(ExitCode::SUCCESS);
        };
        let specs: Vec<String> = section
            .values("fetch")
            .into_iter()
            .map(|v| v.to_str_lossy().into_owned())
            .collect();
        let mut updated = Vec::with_capacity(specs.len());
        let mut changed = false;
        for spec in specs {
            if spec.contains(&context) {
                changed = true;
                updated.push(spec.replace(&context, &replacement));
            } else {
                eprintln!(
                    "warning: Not updating non-default fetch refspec\n\t{spec}\n\tPlease update the configuration manually if necessary."
                );
                updated.push(spec);
            }
        }
        if !changed {
            return Ok(ExitCode::SUCCESS);
        }
        while section.remove("fetch").is_some() {}
        for spec in &updated {
            section.push("fetch", spec.as_str())?;
        }
    }
    persist(&path, &file)?;

    Ok(ExitCode::SUCCESS)
}

/// `compute_renamed_ref()` (builtin/remote.c:620-628): `name` with the
/// component after `refs/remotes/` replaced, whatever `name` is.
fn renamed_ref(name: &str, old: &str, new: &str) -> String {
    const PREFIX: &str = "refs/remotes/";
    match name.get(PREFIX.len()..PREFIX.len() + old.len()) {
        Some(_) => format!("{}{new}{}", &name[..PREFIX.len()], &name[PREFIX.len() + old.len()..]),
        None => name.to_owned(),
    }
}

/// The reference half of `mv()` in git 2.56 (builtin/remote.c:609-756 and
/// :913-942, :987-995) in a reftable repository: when a fetch refspec of the
/// remote maps into `refs/remotes/<old>/`, one transaction deletes every
/// reference there, creates it under the new name without a reflog entry
/// (`REF_SKIP_CREATE_REFLOG`), and copies its reflog entry by entry with the
/// original committers (`ref_transaction_update_reflog()`), followed by a
/// `remote: renamed <old> to <new>` entry — all in iteration order, each
/// entry under its own index.
///
/// `Some(exit code)` when git dies: a prepare it reports as `renaming remote
/// references failed: <reason>` (with the advice for a name conflict), or a
/// failed commit.
fn rename_refs_in_one_transaction(repo: &gix::Repository, old: &str, new: &str) -> Result<Option<ExitCode>> {
    use gix::refs::file::transaction::{prepare, ErrorKind, ReflogUpdate};

    let context = format!(":refs/remotes/{old}/");
    let needs_update = repo
        .config_snapshot()
        .strings(format!("remote.{old}.fetch").as_str())
        .unwrap_or_default()
        .iter()
        .any(|spec| spec.contains_str(context.as_bytes()));
    if !needs_update {
        return Ok(None);
    }

    let null = repo.object_hash().null();
    let committer_info = match repo.committer().transpose()? {
        Some(committer) => {
            let mut info = Vec::new();
            committer.trim().write_to(&mut info)?;
            gix::bstr::BString::from(info)
        }
        None => gix::bstr::BString::default(),
    };
    let mut edits = Vec::new();
    let mut skip_create_reflog = Vec::new();
    let mut reflogs = Vec::new();
    let mut index = 0u64;
    for (name, target) in tracking_refs(repo, old)? {
        let text = name.as_bstr().to_str_lossy().into_owned();
        let new_name = full_name(&renamed_ref(&text, old, new))?;
        let (expected, new_target, oid) = match target {
            Target::Symbolic(referent) => {
                let renamed = full_name(&renamed_ref(&referent.as_bstr().to_str_lossy(), old, new))?;
                (Target::Symbolic(referent), Target::Symbolic(renamed), None)
            }
            Target::Object(id) => (Target::Object(id), Target::Object(id), Some(id)),
        };
        edits.push(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::MustExistAndMatch(expected),
                log: RefLog::AndReference,
                message: Default::default(),
            },
            name: name.clone(),
            deref: false,
        });
        skip_create_reflog.push(edits.len());
        edits.push(RefEdit {
            change: Change::Update {
                log: LogChange::default(),
                expected: PreviousValue::MustNotExist,
                new: new_target,
            },
            name: new_name.clone(),
            deref: false,
        });
        // `rename_one_reflog()`.
        if !crate::refstore::reflog_exists(repo, &text) {
            continue;
        }
        for log in super::refs_migrate::reftable_reflog(repo, &text, &mut index)? {
            reflogs.push(ReflogUpdate {
                name: new_name.clone(),
                old_oid: log.old,
                new_oid: log.new,
                committer_info: log.committer_info().into(),
                message: log.message.as_str().into(),
                index: log.index,
            });
        }
        let id = oid.unwrap_or(null);
        reflogs.push(ReflogUpdate {
            name: new_name.clone(),
            old_oid: id,
            new_oid: id,
            committer_info: committer_info.clone(),
            message: format!("remote: renamed {text} to {}", new_name.as_bstr()).into(),
            index,
        });
        index += 1;
    }

    let fail = gix::lock::acquire::Fail::Immediately;
    let prepared = match repo
        .refs
        .transaction()
        .skip_create_reflog(skip_create_reflog)
        .update_reflogs(reflogs)
        .prepare(edits, fail, fail)
    {
        Ok(prepared) => prepared,
        Err(e) => {
            let (message, conflict) = match &e {
                prepare::Error::Reftable { kind, message } => (message.to_string(), *kind == ErrorKind::NameConflict),
                other => (other.to_string(), false),
            };
            eprintln!("error: renaming remote references failed: {message}");
            if conflict {
                crate::advice::print_hint(CONFLICTING_REMOTE_REFS_ADVICE);
            }
            return Ok(Some(ExitCode::from(128)));
        }
    };
    let committer = repo.committer().transpose()?;
    if prepared.commit(committer).is_err() {
        // `die(_("renaming remote refs failed: %s"), rename.err->buf)`: the
        // buffer is the one queueing used, which holds nothing.
        eprintln!("fatal: renaming remote refs failed: ");
        return Ok(Some(ExitCode::from(128)));
    }
    Ok(None)
}

/// `conflicting_remote_refs_advice` (builtin/remote.c:847-854).
const CONFLICTING_REMOTE_REFS_ADVICE: &str = "\
The remote you are trying to rename has conflicting references in the
new target refspec. This is most likely caused by you trying to nest
a remote into itself, e.g. by renaming 'parent' into 'parent/child'
or by unnesting a remote, e.g. the other way round.

If that is the case, you can address this by first renaming the
remote to a different name.
";

// ---------------------------------------------------------------------------
// remote remove
// ---------------------------------------------------------------------------

/// `git remote remove <name>` — drop the config section, every tracking ref,
/// and the `branch.*` / `remote.pushDefault` settings that named it.
fn remove(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut pos: Vec<&str> = Vec::new();
    for a in args {
        // An empty table is still a table `parse_options()` answers `-h` against,
        // which is why the help test comes before the walk that rejects every
        // other dashed word.
        if let Some(code) = help_requested(a, "", USAGE_REMOVE) {
            return Ok(code);
        }
        // The table is `OPT_END()` alone, so nothing resolves and the walk below
        // rejects every dashed word; the call is here so the option surface is
        // derived from the C table rather than assumed.
        if let Err(code) = canonical(a, OPTS_REMOVE, USAGE_REMOVE) {
            return Ok(code);
        }
        if a.starts_with('-') && a.len() > 1 {
            unknown_option(a);
            return usage(USAGE_REMOVE);
        }
        pos.push(a.as_str());
    }
    if pos.len() != 1 {
        return usage(USAGE_REMOVE);
    }
    let name = pos[0];

    // `remote = remote_get(argv[0])` (builtin/remote.c:1030), which a
    // `remotes/` or `branches/` file satisfies as well.
    read_config(repo);
    if !remote_exists_or_legacy(repo, name) {
        return error(format!("No such remote: '{name}'"), 2);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // `refs_for_each_ref(…, add_branch_for_removal, …)` (builtin/remote.c:1066-1067,
    // 573-608): a ref is removed when this remote's fetch refspecs have it as a
    // destination, no other remote's do, and it lives under `refs/remotes/`; a
    // `refs/heads/` one that qualifies otherwise is only named in the note below.
    let others: Vec<String> = repo
        .remote_names()
        .iter()
        .map(|n| n.to_str_lossy().into_owned())
        .filter(|n| n != name)
        .collect();
    let mut skipped: Vec<String> = Vec::new();
    let mut branches: Vec<FullName> = Vec::new();
    for ref_name in all_ref_names(repo)? {
        let full = ref_name.as_bstr();
        if !remote_find_tracking(repo, name, full) {
            continue;
        }
        // `/* don't delete a branch if another remote also uses it */`
        if others.iter().any(|other| remote_find_tracking(repo, other, full)) {
            continue;
        }
        if !full.starts_with(b"refs/remotes/") {
            if full.starts_with(b"refs/heads/") {
                skipped.push(abbrev_branch(full));
            }
            continue;
        }
        branches.push(ref_name);
    }
    // `refs_delete_refs(…, "remote: remove", &branches, REF_NO_DEREF)` (builtin/remote.c:1070-1073):
    // one transaction for every collected ref, so a reftable store gains a single table.
    if !branches.is_empty() {
        repo.edit_references(branches.into_iter().map(|name| RefEdit {
            change: Change::Delete {
                expected: PreviousValue::Any,
                log: RefLog::AndReference,
                message: "remote: remove".into(),
            },
            name,
            deref: false,
        }))?;
    }
    if !skipped.is_empty() {
        eprintln!(
            "{}",
            match skipped.len() {
                1 => "Note: A branch outside the refs/remotes/ hierarchy was not removed;\nto delete it, use:",
                _ => "Note: Some branches outside the refs/remotes/ hierarchy were not removed;\nto delete them, use:",
            }
        );
        for branch in &skipped {
            eprintln!("  git branch -d {branch}");
        }
    }

    let (path, mut file) = open_local(repo)?;
    let mut section_removed = false;
    while file
        .remove_section("remote", Some(BStr::new(name)))
        .is_some()
    {
        section_removed = true;
    }

    for branch in branch_subsections(&file) {
        let now_empty;
        {
            let Ok(mut section) = file.section_mut("branch", Some(branch.as_bstr())) else {
                continue;
            };
            let mut touched = false;
            if section.value("remote").map(|v| v == name).unwrap_or(false) {
                while section.remove("remote").is_some() {}
                while section.remove("merge").is_some() {}
                touched = true;
            }
            if section
                .value("pushRemote")
                .map(|v| v == name)
                .unwrap_or(false)
            {
                while section.remove("pushRemote").is_some() {}
                touched = true;
            }
            // A section emptied by this removal goes away, as git's does; one
            // that was already empty is left alone.
            now_empty = touched && section.num_values() == 0;
        }
        if now_empty {
            let _ = file.remove_section("branch", Some(branch.as_bstr()));
        }
    }

    // ```c
    // if (repo_config_rename_section(the_repository, buf.buf, NULL) < 1) {
    //         result = error(_("Could not remove config section '%s'"), buf.buf);
    //         goto out;
    // }
    // ```
    //
    // (builtin/remote.c:1092-1096.) A remote read from a legacy file has no section
    // to remove: the `branch.*` settings are gone by then, `remote.pushDefault` stays.
    if !section_removed {
        persist(&path, &file)?;
        return error(format!("Could not remove config section 'remote.{name}'"), 1);
    }

    handle_push_default(repo, &mut file, name, None)?;
    persist(&path, &file)?;

    Ok(ExitCode::SUCCESS)
}

/// `handle_push_default()` (builtin/remote.c:816-845) for a remote renamed to `new`, or
/// removed when `new` is `None`. `config_read_push_default()` (:798-814) remembers the
/// scope and line of the last `remote.pushDefault` naming `old`: one from the command
/// line or the environment is left alone, one in the repository's own files is set to
/// the new name or unset in the repository's file (`local`, the file being edited
/// here), and one in a system or global file draws a warning that it now names a remote
/// that is gone.
fn handle_push_default(repo: &gix::Repository, file: &mut ConfigFile, old: &str, new: Option<&str>) -> Result<()> {
    let snapshot = repo.config_snapshot();
    let mut found: Option<(Source, Option<std::path::PathBuf>)> = None;
    if let Some(sections) = snapshot.plumbing().sections_by_name("remote") {
        for section in sections.filter(|s| s.header().subsection_name().is_none()) {
            if section.values("pushDefault").iter().any(|v| v == old) {
                found = Some((section.meta().source, section.meta().path.clone()));
            }
        }
    }
    let Some((source, origin_path)) = found else {
        return Ok(());
    };
    let scope = match source {
        Source::Local | Source::Worktree => {
            let drop_section = {
                let Ok(mut section) = file.section_mut("remote", None::<&BStr>) else {
                    return Ok(());
                };
                match new {
                    Some(new) => {
                        section.set("pushDefault", new)?;
                        false
                    }
                    None => {
                        while section.remove("pushDefault").is_some() {}
                        section.num_values() == 0
                    }
                }
            };
            if drop_section {
                let _ = file.remove_section("remote", None::<&BStr>);
            }
            return Ok(());
        }
        Source::System | Source::GitInstallation => "system",
        Source::Git | Source::User => "global",
        _ => return Ok(()),
    };
    // `kvi->linenr` of that key: the last line in its file that sets it to `old`.
    let linenr = origin_path
        .and_then(|p| std::fs::read(p).ok())
        .map(|bytes| push_default_line(&bytes, old))
        .unwrap_or(-1);
    eprintln!(
        "warning: The {scope} configuration remote.pushDefault in:\n\tfile:{linenr}\nnow names the non-existent remote '{old}'"
    );
    Ok(())
}

/// The 1-based line of the last `pushDefault = <old>` inside a bare `[remote]`
/// section of a config file, or -1.
fn push_default_line(bytes: &[u8], old: &str) -> i64 {
    let mut in_remote = false;
    let mut found = -1;
    for (i, line) in bytes.split(|&b| b == b'\n').enumerate() {
        let line = line.trim();
        if line.starts_with(b"[") {
            in_remote = line.eq_ignore_ascii_case(b"[remote]");
            continue;
        }
        let Some((key, value)) = line.split_once_str("=") else {
            continue;
        };
        if in_remote && key.trim().eq_ignore_ascii_case(b"pushdefault") && value.trim().trim_with(|c| c == '"') == old.as_bytes() {
            found = i as i64 + 1;
        }
    }
    found
}

// ---------------------------------------------------------------------------
// remote set-head / set-branches
// ---------------------------------------------------------------------------

/// `git remote set-head <name> (-a | -d | <branch>)` — point (or drop) the
/// `refs/remotes/<name>/HEAD` symref. `-a` contacts the remote and derives the
/// branch from its advertised HEAD.
fn set_head(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    // `struct ref_store *refs = get_main_ref_store(the_repository);` is a
    // declaration initializer (builtin/remote.c:1552), so the store is built
    // before `parse_options()` runs: even `set-head -h` dies on a refused
    // `core.logAllRefUpdates`.
    get_main_ref_store(repo);
    let mut auto = false;
    let mut delete = false;
    let mut pos: Vec<&str> = Vec::new();
    for orig in args {
        if let Some(code) = help_requested(orig, "ad", USAGE_SET_HEAD) {
            return Ok(code);
        }
        let resolved = match canonical(orig, OPTS_SET_HEAD, USAGE_SET_HEAD) {
            Ok(name) => name,
            Err(code) => return Ok(code),
        };
        match resolved.as_ref() {
            "-a" | "--auto" => auto = true,
            "--no-auto" => auto = false,
            "-d" | "--delete" => delete = true,
            "--no-delete" => delete = false,
            s if s.starts_with('-') && s.len() > 1 => {
                unknown_option(s);
                return usage(USAGE_SET_HEAD);
            }
            _ => pos.push(orig.as_str()),
        }
    }
    // `/* All modes require at least a remote name. */` — then `remote_get(argv[0])`,
    // whose read of a legacy file warns, and only then the three modes, each with its
    // own argument count (builtin/remote.c:1565-1592).
    if pos.is_empty() {
        return usage(USAGE_SET_HEAD);
    }
    let name = pos[0];
    let _ = legacy(repo, name);
    let one_mode = match (auto, delete) {
        (false, false) => pos.len() == 2,
        (true, false) | (false, true) => pos.len() == 1,
        (true, true) => false,
    };
    if !one_mode {
        return usage(USAGE_SET_HEAD);
    }
    let head = format!("refs/remotes/{name}/HEAD");

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if delete {
        if repo.try_find_reference(head.as_str())?.is_some() {
            delete_ref(repo, full_name(&head)?)?;
        }
        return Ok(ExitCode::SUCCESS);
    }
    if auto {
        let map = match query_ref_map(repo, name) {
            Ok(map) => map,
            Err(e) => {
                if let Some(code) = crate::transport_err::file_url_fatal_in(&e) {
                    return Ok(code);
                }
                return fatal(e);
            }
        };
        let heads = remote_head_names(&map);
        if heads.is_empty() {
            return error("Cannot determine remote HEAD", 1);
        }
        if heads.len() > 1 {
            eprintln!("error: Multiple remote HEAD branches. Please choose one explicitly with:");
            for h in &heads {
                eprintln!("  git remote set-head {name} {h}");
            }
            return Ok(ExitCode::from(1));
        }
        let head_name = &heads[0];
        let target = format!("refs/remotes/{name}/{head_name}");
        if repo.try_find_reference(target.as_str())?.is_none() {
            return error(format!("Not a valid ref: {target}"), 1);
        }
        let (prev, was_detached) = symref_prev(repo, head.as_str())?;
        let previous = resolved_id(repo, head.as_str())?;
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "remote set-head".into(),
                },
                expected: PreviousValue::Any,
                new: Target::Symbolic(full_name(&target)?),
            },
            name: full_name(&head)?,
            deref: false,
        })?;
        log_symref_update(repo, head.as_str(), previous, "remote set-head")?;
        report_set_head_auto(name, head_name, &prev, was_detached);
        return Ok(ExitCode::SUCCESS);
    }

    let branch = pos[1];
    let target = format!("refs/remotes/{name}/{branch}");
    if repo.try_find_reference(target.as_str())?.is_none() {
        return error(format!("Not a valid ref: {target}"), 1);
    }
    let previous = resolved_id(repo, head.as_str())?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "remote set-head".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(full_name(&target)?),
        },
        name: full_name(&head)?,
        deref: false,
    })?;
    log_symref_update(repo, head.as_str(), previous, "remote set-head")?;
    Ok(ExitCode::SUCCESS)
}

/// `git remote set-branches [--add] <name> <branch>…` — rewrite (or extend)
/// the fetch refspecs so only the named branches are tracked.
fn set_branches(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut append = false;
    let mut pos: Vec<&str> = Vec::new();
    for orig in args {
        if let Some(code) = help_requested(orig, "", USAGE_SET_BRANCHES) {
            return Ok(code);
        }
        let resolved = match canonical(orig, OPTS_SET_BRANCHES, USAGE_SET_BRANCHES) {
            Ok(name) => name,
            Err(code) => return Ok(code),
        };
        match resolved.as_ref() {
            "--add" => append = true,
            "--no-add" => append = false,
            s if s.starts_with('-') && s.len() > 1 => {
                unknown_option(s);
                return usage(USAGE_SET_BRANCHES);
            }
            _ => pos.push(orig.as_str()),
        }
    }
    let Some(name) = pos.first().copied() else {
        eprintln!("error: no remote specified");
        return usage(USAGE_SET_BRANCHES);
    };
    // `set_remote_branches()` → `remote_get(remotename)` (builtin/remote.c:1768).
    read_config(repo);
    if !remote_exists_or_legacy(repo, name) {
        return error(format!("No such remote '{name}'"), 2);
    }
    let mirror = repo
        .config_snapshot()
        .boolean(format!("remote.{name}.mirror").as_str())
        .unwrap_or(false);

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let (path, mut file) = open_local(repo)?;
    // ```c
    // if (!add_mode && remove_all_fetch_refspecs(key.buf)) {
    //         strbuf_release(&key);
    //         return 1;
    // }
    // ```
    //
    // (builtin/remote.c:1774-1777.) `remove_all_fetch_refspecs()` is the gently
    // `MULTI_REPLACE` unset, which answers `CONFIG_NOTHING_SET` when the repository's
    // file holds no `remote.<name>.fetch`: exit 1, nothing said, nothing added.
    if !append && unset_matching(&mut file, name, "fetch", |_| true) == 0 {
        return Ok(ExitCode::from(1));
    }
    // `add_branches()` hands `remote->mirror` to `add_branch()` (builtin/remote.c:126-138,
    // :1747-1758), so a mirror's branches map onto themselves.
    {
        let mut section = file.section_mut_or_create_new("remote", Some(BStr::new(name)))?;
        for branch in &pos[1..] {
            let spec = match mirror {
                true => format!("+refs/{branch}:refs/{branch}"),
                false => format!("+refs/heads/{branch}:refs/remotes/{name}/{branch}"),
            };
            section.push("fetch", spec.as_str())?;
        }
    }
    persist(&path, &file)?;
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// remote get-url / set-url
// ---------------------------------------------------------------------------

/// `git remote get-url [--push] [--all] <name>`.
fn get_url(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut push = false;
    let mut all = false;
    let mut pos: Vec<&str> = Vec::new();
    for a in args {
        if let Some(code) = help_requested(a, "", USAGE_GET_URL) {
            return Ok(code);
        }
        let resolved = match canonical(a, OPTS_GET_URL, USAGE_GET_URL) {
            Ok(name) => name,
            Err(code) => return Ok(code),
        };
        match resolved.as_ref() {
            "--push" => push = true,
            "--no-push" => push = false,
            "--all" => all = true,
            "--no-all" => all = false,
            s if s.starts_with('-') && s.len() > 1 => {
                unknown_option(s);
                return usage(USAGE_GET_URL);
            }
            _ => pos.push(a.as_str()),
        }
    }
    if pos.len() != 1 {
        return usage(USAGE_GET_URL);
    }
    let name = pos[0];
    // `remote = remote_get(remotename)` (builtin/remote.c:1826).
    read_config(repo);
    if !remote_exists_or_legacy(repo, name) {
        return error(format!("No such remote '{name}'"), 2);
    }

    let urls = if push {
        push_urls_or_name(repo, name)
    } else {
        fetch_urls_or_name(repo, name)
    };
    for url in urls.iter().take(if all { urls.len() } else { 1 }) {
        println!("{url}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `git remote set-url` in its three forms.
///
/// `--delete <url>` treats its argument as an extended (POSIX ERE) regular
/// expression, matching git's `regcomp(&old_regex, oldurl, REG_EXTENDED)`.
fn set_url(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut push = false;
    let mut append = false;
    let mut delete = false;
    let mut pos: Vec<&str> = Vec::new();
    for a in args {
        if let Some(code) = help_requested(a, "", USAGE_SET_URL) {
            return Ok(code);
        }
        let resolved = match canonical(a, OPTS_SET_URL, USAGE_SET_URL) {
            Ok(name) => name,
            Err(code) => return Ok(code),
        };
        match resolved.as_ref() {
            "--push" => push = true,
            "--no-push" => push = false,
            "--add" => append = true,
            "--no-add" => append = false,
            "--delete" => delete = true,
            "--no-delete" => delete = false,
            s if s.starts_with('-') && s.len() > 1 => {
                unknown_option(s);
                return usage(USAGE_SET_URL);
            }
            _ => pos.push(a.as_str()),
        }
    }
    // `if (add_mode && delete_mode) die(_("--add --delete doesn't make sense"));`, ahead
    // of the argument count (builtin/remote.c:1869-1870).
    if append && delete {
        return fatal("--add --delete doesn't make sense");
    }
    let well_formed = if append || delete {
        pos.len() == 2
    } else {
        pos.len() == 2 || pos.len() == 3
    };
    if !well_formed {
        return usage(USAGE_SET_URL);
    }
    let name = pos[0];
    let value = pos[1];
    // `remote = remote_get(remotename)` (builtin/remote.c:1883), which a legacy
    // `remotes/` or `branches/` file satisfies as well.
    read_config(repo);
    if !remote_exists_or_legacy(repo, name) {
        return error(format!("No such remote '{name}'"), 2);
    }

    let key = if push { "pushurl" } else { "url" };

    // ```c
    // if (delete_mode)
    //         oldurl = newurl;
    // …
    // /* Special cases that add new entry. */
    // if ((!oldurl && !delete_mode) || add_mode) {
    //         if (add_mode)
    //                 repo_config_set_multivar(the_repository, name_buf.buf, newurl, "^$", 0);
    //         else
    //                 repo_config_set(the_repository, name_buf.buf, newurl);
    //         goto out;
    // }
    //
    // /* Old URL specified. Demand that one matches. */
    // if (regcomp(&old_regex, oldurl, REG_EXTENDED))
    //         die(_("Invalid old URL pattern: %s"), oldurl);
    //
    // for (size_t i = 0; i < urlset->nr; i++)
    //         if (!regexec(&old_regex, urlset->v[i], 0, NULL, 0))
    //                 matches++;
    //         else
    //                 negative_matches++;
    // if (!delete_mode && !matches)
    //         die(_("No such URL found: %s"), oldurl);
    // if (delete_mode && !negative_matches && !push_mode)
    //         die(_("Will not delete all non-push URLs"));
    //
    // if (!delete_mode)
    //         repo_config_set_multivar(the_repository, name_buf.buf, newurl, oldurl, 0);
    // else
    //         repo_config_set_multivar(the_repository, name_buf.buf, NULL, oldurl,
    //                                  CONFIG_FLAGS_MULTI_REPLACE);
    // ```
    //
    // (builtin/remote.c:1880-1927.) Two things this port had wrong: `<oldurl>`
    // is an *extended regular expression* that every matching URL is rewritten
    // through — `git remote set-url --push <name> foo qu+x` renames `quux` —
    // and a pattern that matches nothing is fatal. Comparing `<oldurl>`
    // literally against the last value instead meant
    // `git remote set-url someremote zot bar` silently did nothing and exited 0
    // where git dies with `No such URL found: bar`.
    let oldurl: Option<&str> = match delete {
        true => Some(value),
        false => pos.get(2).copied(),
    };

    // `regcomp(&old_regex, oldurl, REG_EXTENDED)` and `regexec()` over `urlset`:
    // `remote->url` / `remote->pushurl`, the merged configuration's list.
    if let Some(oldurl) = oldurl.filter(|_| !append) {
        let Some(re) = super::pack_objects::PosixRegex::extended(oldurl) else {
            return fatal(format!("Invalid old URL pattern: {oldurl}"));
        };
        let urlset = effective_urls(repo, name, key);
        let matches = urlset.iter().filter(|u| re.is_match(u.as_bytes())).count();
        let negative_matches = urlset.len() - matches;
        if !delete && matches == 0 {
            return fatal(format!("No such URL found: {oldurl}"));
        }
        if delete && negative_matches == 0 && !push {
            return fatal("Will not delete all non-push URLs");
        }
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // The write is `repo_config_set_multivar()` (config.c:3028-3290) on the repository's
    // own file: `--add` appends (value pattern `^$`), the others edit the values that
    // match the value pattern where they stand — `repo_config_set()` has none and so
    // matches every value — and a value pattern opening with `!` matches what the rest
    // does not. Without `CONFIG_FLAGS_MULTI_REPLACE` a second match is
    // `warning: <key> has multiple values` (`store_aux()`, :2686-2689) and nothing is
    // written; a delete is the `MULTI_REPLACE` unset of every match, and one that
    // matches nothing is `CONFIG_NOTHING_SET`. Both refusals die in the non-gently
    // wrapper (:3295-3302).
    let full_key = format!("remote.{name}.{key}");
    let (path, mut file) = open_local(repo)?;
    let value_pattern: Option<(bool, super::pack_objects::PosixRegex)> = match (append, oldurl) {
        (false, Some(pattern)) => {
            let (negate, pattern) = match pattern.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, pattern),
            };
            match super::pack_objects::PosixRegex::extended(pattern) {
                Some(re) => Some((negate, re)),
                None => {
                    eprintln!("error: invalid pattern: {pattern}");
                    return fatal(format!("could not set '{full_key}' to '{value}'"));
                }
            }
        }
        _ => None,
    };
    let matching: Vec<usize> = match (append, file.raw_values_mut_by("remote", Some(BStr::new(name)), key)) {
        (false, Ok(values)) => values
            .get()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .filter(|(_, v)| match &value_pattern {
                Some((negate, re)) => re.is_match(v) != *negate,
                None => true,
            })
            .map(|(i, _)| i)
            .collect(),
        _ => Vec::new(),
    };
    if delete {
        let removed = unset_matching(&mut file, name, key, |v| match &value_pattern {
            Some((negate, re)) => re.is_match(v) != *negate,
            None => true,
        });
        if removed == 0 {
            return fatal(format!("could not unset '{full_key}'"));
        }
    } else {
        match matching.as_slice() {
            [] => {
                file.section_mut_or_create_new("remote", Some(BStr::new(name)))?
                    .push(key, value)?;
            }
            [one] => {
                file.raw_values_mut_by("remote", Some(BStr::new(name)), key)?
                    .set_at(*one, value)?;
            }
            _ => {
                eprintln!("warning: remote.{name}.{} has multiple values", key.to_ascii_lowercase());
                return fatal(format!("could not set '{full_key}' to '{value}'"));
            }
        }
    }
    persist(&path, &file)?;
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// remote show
// ---------------------------------------------------------------------------

/// `git remote show [-n] [<name>…]`.
///
/// Without a name this is a plain listing, exactly like `git remote [-v]`;
/// with names it renders one detail block per remote. An unknown name is not
/// an error: git synthesizes a block whose URLs fall back to the name itself.
fn show(repo: &gix::Repository, args: &[String], verbose: bool) -> Result<ExitCode> {
    let mut no_query = false;
    let mut names: Vec<&str> = Vec::new();
    for a in args {
        if let Some(code) = help_requested(a, "n", USAGE_SHOW) {
            return Ok(code);
        }
        // `show`'s only option is `OPT_BOOL('n', NULL, ...)` — short-only, so
        // there is no long spelling at all and `--no-query` is as unknown to
        // stock git as any other `--x` (verified against 2.55.0:
        // ``error: unknown option `no-query'``, exit 129).
        let resolved = match canonical(a, OPTS_SHOW, USAGE_SHOW) {
            Ok(name) => name,
            Err(code) => return Ok(code),
        };
        match resolved.as_ref() {
            "-n" => no_query = true,
            s if s.starts_with('-') && s.len() > 1 => {
                unknown_option(s);
                return usage(USAGE_SHOW);
            }
            _ => names.push(a.as_str()),
        }
    }
    if names.is_empty() {
        return list(repo, verbose);
    }
    for name in &names {
        // `get_remote_ref_states()` opens with `remote_get(name)`
        // (builtin/remote.c:1151), ahead of the transport and every line printed.
        read_config(repo);
        // Without `-n`, contact the remote once; a failure is fatal, exactly as
        // git's `transport_get_remote_refs` dies.
        // ```c
        // states.remote = remote_get(*argv);
        // if (!states.remote)
        //         return error(_("No such remote: '%s'"), *argv);
        // ```
        //
        // (`show()`, builtin/remote.c:1200-1203.) `remote_get()` does *not* fail
        // on a name nothing configures: `remote_get_1()` makes an anonymous remote
        // whose one URL is the name itself, so the `No such remote` arm is
        // unreachable for `git remote show <anything>` and the argument is
        // *connected to* instead. A name that is neither a configured remote nor
        // a repository on disk therefore ends where every other unreachable
        // transport does.
        if !no_query && repo.find_remote(*name).is_err() {
            if let Some(bad) = super::send_pack::local_dest_that_is_not_a_repository(name, crate::setup::setup_cwd(repo).as_deref()) {
                return Ok(crate::transport_err::enter_repo_fatal(&bad));
            }
        }
        let map = if no_query {
            None
        } else {
            match query_ref_map(repo, name) {
                Ok(map) => Some(map),
                Err(e) => {
                    // An ssh transport that never connected is git's own `die()`.
                    let url = fetch_urls_or_name(repo, name)
                        .first()
                        .cloned()
                        .unwrap_or_else(|| name.to_string());
                    if let Some(code) = crate::transport_err::ssh_fatal(&url, &e) {
                        return Ok(code);
                    }
                    if let Some(code) = crate::transport_err::file_url_fatal_in(&e) {
                        return Ok(code);
                    }
                    eprintln!("fatal: {e}");
                    return Ok(ExitCode::from(128));
                }
            }
        };
        show_one(repo, name, map.as_ref())?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Render the detail block for a single remote. With `map` present the HEAD
/// branch and per-branch status come from the advertisement; without it the
/// offline (`-n`) form is rendered.
fn show_one(
    repo: &gix::Repository,
    name: &str,
    map: Option<&gix::remote::fetch::RefMap>,
) -> Result<()> {
    let fetch = fetch_urls_or_name(repo, name);
    let push = push_urls_or_name(repo, name);

    println!("* remote {name}");
    println!(
        "  Fetch URL: {}",
        fetch.first().map_or(name, String::as_str)
    );
    println!("  Push  URL: {}", push.first().map_or(name, String::as_str));

    match map {
        None => {
            println!("  HEAD branch: (not queried)");

            // Remote-tracking branches selected by the fetch refspecs.
            let branches = remote_branches(repo, name)?;
            if !branches.is_empty() {
                if branches.len() == 1 {
                    println!("  Remote branch: (status not queried)");
                } else {
                    println!("  Remote branches: (status not queried)");
                }
                for b in &branches {
                    println!("    {b}");
                }
            }
        }
        Some(map) => {
            let heads = remote_head_names(map);
            match heads.len() {
                0 => println!("  HEAD branch: (unknown)"),
                1 => println!("  HEAD branch: {}", heads[0]),
                _ => {
                    println!(
                        "  HEAD branch (remote HEAD is ambiguous, may be one of the following):"
                    );
                    for h in &heads {
                        println!("    {h}");
                    }
                }
            }

            let rows = remote_branch_states(repo, name, map)?;
            if !rows.is_empty() {
                if rows.len() == 1 {
                    println!("  Remote branch:");
                } else {
                    println!("  Remote branches:");
                }
                let width = rows
                    .iter()
                    .map(|(n, _)| n.chars().count())
                    .max()
                    .unwrap_or(0);
                for (bname, status) in &rows {
                    println!("    {bname:<width$}{status}");
                }
            }
        }
    }

    // Local branches configured for `git pull` (branch.<b>.remote == <name>).
    let pulls = pull_config(repo, name)?;
    if !pulls.is_empty() {
        if pulls.len() == 1 {
            println!("  Local branch configured for 'git pull':");
        } else {
            println!("  Local branches configured for 'git pull':");
        }
        let width = pulls
            .iter()
            .map(|(b, _, _)| b.chars().count())
            .max()
            .unwrap_or(0);
        // `branch_info->rebase >= REBASE_TRUE` (builtin/remote.c:1246-1247, :1261-1282).
        use super::pull::RebaseMode;
        let rebases = |mode: &Option<RebaseMode>| {
            matches!(mode, Some(RebaseMode::Plain | RebaseMode::Merges | RebaseMode::Interactive))
        };
        let any_rebase = pulls.iter().any(|(_, rebase, _)| rebases(rebase));

        for (bname, rebase, merges) in &pulls {
            let first = merges.first().map(String::as_str).unwrap_or("");
            if rebases(rebase) && merges.len() > 1 {
                eprintln!("error: invalid branch.{bname}.merge; cannot rebase onto > 1 branch");
            } else if rebases(rebase) {
                let msg = match rebase {
                    Some(RebaseMode::Interactive) => "rebases interactively onto remote",
                    Some(RebaseMode::Merges) => "rebases interactively (with merges) onto remote",
                    _ => "rebases onto remote",
                };
                println!("    {bname:<width$} {msg} {first}");
            } else if any_rebase {
                // When any listed branch rebases, git shifts the merge verb by one column.
                println!("    {bname:<width$}  merges with remote {first}");
                let also = "    and with remote";
                for m in merges.iter().skip(1) {
                    println!("    {pad:<width$} {also} {m}", pad = "");
                }
            } else {
                println!("    {bname:<width$} merges with remote {first}");
                let also = "   and with remote";
                for m in merges.iter().skip(1) {
                    println!("    {pad:<width$} {also} {m}", pad = "");
                }
            }
        }
    }

    show_push(repo, name, map);
    Ok(())
}

/// The `'git push'` section of a `show` block: mirror note, explicit refspecs,
/// or the default matching behaviour.
fn show_push(repo: &gix::Repository, name: &str, map: Option<&gix::remote::fetch::RefMap>) {
    let cfg = repo.config_snapshot();
    let mirror = cfg
        .plumbing()
        .boolean_by("remote", Some(BStr::new(name)), "mirror")
        .ok()
        .flatten()
        .unwrap_or(false);
    if mirror {
        println!("  Local refs will be mirrored by 'git push'");
        return;
    }

    let specs = effective_specs(repo, name, "push");

    // With the advertisement in hand, `get_push_ref_states()` runs the same
    // `match_push_refs()` a push would and reports what each update *would* do.
    // Without it (`-n`), git falls back to naming the refspecs alone.
    let rows: Vec<PushRow> = match map {
        Some(map) => push_states(repo, &specs, map),
        None => {
            if specs.is_empty() {
                println!("  Local ref configured for 'git push' (status not queried):");
                println!("    (matching) pushes to (matching)");
                return;
            }
            specs
                .iter()
                .map(|spec| {
                    let forced = spec.starts_with('+');
                    let body = spec.strip_prefix('+').unwrap_or(spec);
                    let (src, dst) = match body.split_once(':') {
                        Some((s, d)) => (s.to_string(), d.to_string()),
                        None => (body.to_string(), body.to_string()),
                    };
                    let shown = if src.is_empty() { "(delete)".to_string() } else { src.clone() };
                    PushRow { sort: src, width_key: shown.clone(), src: shown, forced, dst, status: None }
                })
                .collect()
        }
    };
    if rows.is_empty() {
        return;
    }

    let queried = map.is_some();
    let suffix = if queried { "" } else { " (status not queried)" };
    if rows.len() == 1 {
        println!("  Local ref configured for 'git push'{suffix}:");
    } else {
        println!("  Local refs configured for 'git push'{suffix}:");
    }
    let src_width = rows.iter().map(|r| r.width_key.chars().count()).max().unwrap_or(0);
    let dst_width = rows.iter().map(|r| r.dst.chars().count()).max().unwrap_or(0);
    for r in &rows {
        let verb = if r.forced { "forces to" } else { "pushes to" };
        match &r.status {
            // `show_push_info_item()`: both columns padded, then the status.
            Some(status) => println!(
                "    {src:<src_width$} {verb} {dst:<dst_width$} ({status})",
                src = r.src,
                dst = r.dst
            ),
            None => println!("    {src:<src_width$} {verb} {dst}", src = r.src, dst = r.dst),
        }
    }
}

/// One line of the `'git push'` block.
struct PushRow {
    /// The key git sorts on: the source ref, so a deletion sorts first.
    sort: String,
    /// The source as displayed: the ref name, or `(none)` for a deletion in the
    /// queried form and `(delete)` in the `-n` one.
    src: String,
    /// What the source column is *measured* by. git stores a deletion under the
    /// key `(delete)` and prints `(none)` in its place, so the column keeps the
    /// wider of the two even when only the shorter one is shown.
    width_key: String,
    forced: bool,
    dst: String,
    /// `None` under `-n`, where no status was queried.
    status: Option<&'static str>,
}

/// `get_push_ref_states()`: pair each local ref the push refspecs select with the
/// remote ref it would update, and say what the update would be.
///
/// With no configured refspec the default is `match_push_refs(..., MATCH_REFS_NONE)`,
/// which pairs local branches with *already existing* same-named remote refs and
/// leaves a local-only branch out — the same set a bare `git push` would send.
fn push_states(
    repo: &gix::Repository,
    specs: &[String],
    map: &gix::remote::fetch::RefMap,
) -> Vec<PushRow> {
    // What the remote advertises, by full ref name.
    let remote: std::collections::BTreeMap<String, gix::hash::ObjectId> = map
        .remote_refs
        .iter()
        .filter_map(|r| {
            let (name, target, peeled) = r.unpack();
            Some((name.to_str_lossy().into_owned(), peeled.or(target)?.to_owned()))
        })
        .collect();
    // Local branches, by full ref name.
    let mut local: Vec<(String, gix::hash::ObjectId)> = Vec::new();
    if let Ok(refs) = repo.references() {
        if let Ok(iter) = refs.prefixed("refs/heads/") {
            for r in iter.filter_map(Result::ok) {
                let name = r.name().as_bstr().to_str_lossy().into_owned();
                if let Some(id) = r.try_id() {
                    local.push((name, id.detach()));
                }
            }
        }
    }

    let mut rows: Vec<PushRow> = Vec::new();
    if specs.is_empty() {
        // The matching default.
        for (name, new) in &local {
            let Some(old) = remote.get(name) else { continue };
            let shown = abbrev_branch(name.as_bytes().as_bstr());
            rows.push(PushRow {
                sort: name.clone(),
                width_key: shown.clone(),
                src: shown.clone(),
                forced: false,
                dst: shown,
                status: Some(push_status(repo, Some(*old), Some(*new))),
            });
        }
    } else {
        for spec in specs {
            let forced = spec.starts_with('+');
            let body = spec.strip_prefix('+').unwrap_or(spec);
            let (src, dst) = match body.split_once(':') {
                Some((s, d)) => (s.to_string(), d.to_string()),
                None => (body.to_string(), body.to_string()),
            };
            if src.is_empty() {
                // A deletion refspec: the destination goes away.
                let full_dst = full_head_name(&dst);
                rows.push(PushRow {
                    sort: String::new(),
                    width_key: "(delete)".into(),
                    src: "(none)".into(),
                    forced,
                    dst: abbrev_branch(full_dst.as_bytes().as_bstr()),
                    status: Some("delete"),
                });
                continue;
            }
            // A pattern refspec expands to one row per matching local ref.
            if let (Some(src_stem), Some(dst_stem)) = (src.strip_suffix('*'), dst.strip_suffix('*')) {
                for (name, new) in &local {
                    let Some(tail) = name.strip_prefix(src_stem) else { continue };
                    let full_dst = format!("{dst_stem}{tail}");
                    let old = remote.get(&full_dst).copied();
                    let shown = abbrev_branch(name.as_bytes().as_bstr());
                    rows.push(PushRow {
                        sort: name.clone(),
                        width_key: shown.clone(),
                        src: shown,
                        forced,
                        dst: abbrev_branch(full_dst.as_bytes().as_bstr()),
                        status: Some(push_status(repo, old, Some(*new))),
                    });
                }
                continue;
            }
            let full_src = full_head_name(&src);
            let full_dst = full_head_name(&dst);
            let old = remote.get(&full_dst).copied();
            // `match_explicit_lhs()` (remote.c:1181-1203): a source no local branch
            // names is `try_explicit_object_name()` (:1130-1149) — any object name
            // `repo_get_oid()` resolves, `HEAD` included — shown as it was written.
            let (new, shown) = match local.iter().find(|(n, _)| *n == full_src) {
                Some((_, id)) => (Some(*id), abbrev_branch(full_src.as_bytes().as_bstr())),
                None => match repo.rev_parse_single(src.as_str()) {
                    Ok(id) => (Some(id.detach()), src.clone()),
                    Err(_) => (None, abbrev_branch(full_src.as_bytes().as_bstr())),
                },
            };
            rows.push(PushRow {
                sort: full_src.clone(),
                width_key: shown.clone(),
                src: shown,
                forced,
                dst: abbrev_branch(full_dst.as_bytes().as_bstr()),
                status: Some(push_status(repo, old, new)),
            });
        }
    }
    rows.sort_by(|a, b| a.sort.cmp(&b.sort));
    rows
}

/// A short ref name as a full one, when it is not already qualified.
fn full_head_name(name: &str) -> String {
    if name.starts_with("refs/") {
        name.to_owned()
    } else {
        format!("refs/heads/{name}")
    }
}

/// `get_push_ref_states()`'s five verdicts, in its order of testing.
fn push_status(
    repo: &gix::Repository,
    old: Option<gix::hash::ObjectId>,
    new: Option<gix::hash::ObjectId>,
) -> &'static str {
    let Some(old) = old else { return "create" };
    let Some(new) = new else { return "delete" };
    if old == new {
        return "up to date";
    }
    // `ref_newer()`: the update fast-forwards when the remote tip is an ancestor
    // of the local one. An old tip this repository does not have cannot be
    // compared, and git calls that out of date.
    let fast_forward = repo.find_object(old).is_ok()
        && repo
            .merge_base(new, old)
            .map(|base| base.detach() == old)
            .unwrap_or(false);
    if fast_forward {
        "fast-forwardable"
    } else {
        "local out of date"
    }
}

/// The remote branches `show -n` lists: `append_ref_to_tracked_list()`
/// (builtin/remote.c) runs every local ref that is not a symref through
/// `remote_find_tracking()` as a destination and keeps the source it maps back
/// from, `abbrev_branch()`ed — so the tracking refs need not live under
/// `refs/remotes/<name>/`, and a refspec landing a tag in `refs/heads/` lists
/// `refs/tags/<t>`. Sorted, as the list is shown.
fn remote_branches(repo: &gix::Repository, name: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for reference in repo.refs.iter()?.all()? {
        let ref_name = match reference {
            Ok(reference) if matches!(reference.target, Target::Symbolic(_)) => continue,
            Ok(reference) => reference.name,
            // A ref whose file cannot be parsed is still visited, as a plain one.
            Err(gix::refs::file::iter::loose_then_packed::Error::ReferenceCreation { relative_path, .. }) => {
                match FullName::try_from(gix::path::into_bstr(relative_path).as_ref()) {
                    Ok(broken) => broken,
                    Err(_) => continue,
                }
            }
            Err(e) => anyhow::bail!("{e}"),
        };
        if let Some(src) = tracking_source(repo, name, ref_name.as_bstr()) {
            out.push(abbrev_branch(src.as_bstr()));
        }
    }
    out.sort();
    Ok(out)
}

/// Port of git's `get_ref_states`: each remote-tracking branch of `<name>`
/// paired with its live status suffix, as `(display, status)` sorted by
/// display name (git merges the four state lists through a sorted, de-duplicated
/// `string_list`).
///
/// `new`/`tracked` entries carry the short remote-side name (`abbrev_branch` of
/// the advertised ref); `stale` entries keep the full `refs/remotes/<name>/…`
/// path, exactly as git's `abbrev_branch` leaves a non-`refs/heads/` name.
/// A name present in more than one state takes the first that matches in git's
/// order (new, then tracked, then stale — negative-refspec `skipped` refs are
/// not modelled here).
fn remote_branch_states(
    repo: &gix::Repository,
    name: &str,
    map: &gix::remote::fetch::RefMap,
) -> Result<Vec<(String, String)>> {
    let mut new_set: BTreeSet<String> = BTreeSet::new();
    let mut tracked_set: BTreeSet<String> = BTreeSet::new();

    // Each mapping is a remote ref the fetch refspecs selected; it is `tracked`
    // when its local counterpart already exists, else `new`.
    //
    // `get_ref_states()` walks `get_fetch_map(remote_refs, remote->fetch, …)`,
    // which is the remote's *configured* refspecs and nothing else. gitoxide adds
    // an implicit tag refspec on top (the `tagOpt` default), and its mappings must
    // be left out or every advertised tag would be listed as a remote branch.
    for m in &map.mappings {
        if m.spec_index.implicit_index().is_some() {
            continue;
        }
        let Some(remote_name) = m.remote.as_name() else {
            continue;
        };
        if remote_name == "HEAD" {
            continue;
        }
        let display = abbrev_branch(remote_name);
        let local_exists = m.local.as_ref().is_some_and(|l| {
            repo.try_find_reference(l.to_str_lossy().as_ref())
                .ok()
                .flatten()
                .is_some()
        });
        if local_exists {
            tracked_set.insert(display);
        } else {
            new_set.insert(display);
        }
    }

    // Stale (git's `get_stale_heads`): a local tracking ref selected by a fetch
    // refspec that the remote no longer advertises. Symbolic refs (HEAD) are
    // skipped, matching `REF_ISSYMREF`.
    let live_locals: BTreeSet<String> = map
        .mappings
        .iter()
        .filter_map(|m| m.local.as_ref())
        .map(|l| l.to_str_lossy().into_owned())
        .collect();
    let dsts = fetch_refspec_dsts(repo, name);
    let mut stale_set: BTreeSet<String> = BTreeSet::new();
    for (ref_name, target) in tracking_refs(repo, name)? {
        if matches!(target, Target::Symbolic(_)) {
            continue;
        }
        let full = ref_name.as_bstr().to_str_lossy().into_owned();
        if !dsts.iter().any(|d| refspec_dst_matches(d, &full)) {
            continue;
        }
        if live_locals.contains(&full) {
            continue;
        }
        stale_set.insert(full);
    }

    let mut names: BTreeSet<&String> = BTreeSet::new();
    names.extend(&new_set);
    names.extend(&tracked_set);
    names.extend(&stale_set);

    let mut out = Vec::with_capacity(names.len());
    for n in names {
        let status = if new_set.contains(n) {
            format!(" new (next fetch will store in remotes/{name})")
        } else if tracked_set.contains(n) {
            " tracked".to_string()
        } else {
            " stale (use 'git remote prune' to remove)".to_string()
        };
        out.push((n.clone(), status));
    }
    Ok(out)
}

/// Local branches (sorted) whose `branch.<b>.remote` is `<name>` and that have
/// at least one `branch.<b>.merge`, returned as `(branch, rebase, merges)` with
/// merge refs shortened (`refs/heads/` stripped).
/// Each branch's `info->rebase` as `read_branches()` leaves it: `config_read_branches()`
/// (builtin/remote.c:346-355) runs `rebase_parse_value()` on every `branch.<name>.rebase`
/// in configuration order, the last one standing, and warns about each value it cannot
/// read — once, as `read_branches()` reads the configuration only once. `None` is
/// `REBASE_INVALID`, which the `>= REBASE_TRUE` tests read as false.
fn branch_rebase_modes(repo: &gix::Repository) -> &'static std::collections::HashMap<BString, Option<super::pull::RebaseMode>> {
    static MODES: std::sync::OnceLock<std::collections::HashMap<BString, Option<super::pull::RebaseMode>>> =
        std::sync::OnceLock::new();
    MODES.get_or_init(|| {
        let mut modes = std::collections::HashMap::new();
        let snapshot = repo.config_snapshot();
        if let Some(sections) = snapshot.plumbing().sections_by_name("branch") {
            for section in sections {
                let Some(name) = section.header().subsection_name() else {
                    continue;
                };
                for value in section.values("rebase") {
                    let value = value.to_str_lossy();
                    let mode = super::pull::rebase_parse_value(&value);
                    if mode.is_none() {
                        eprintln!("warning: unhandled branch.{name}.rebase={value}; assuming 'true'");
                    }
                    modes.insert(name.to_owned(), mode);
                }
            }
        }
        modes
    })
}

fn pull_config(
    repo: &gix::Repository,
    name: &str,
) -> Result<Vec<(String, Option<super::pull::RebaseMode>, Vec<String>)>> {
    let cfg = repo.config_snapshot();
    let file = cfg.plumbing();

    // Collect every configured branch name (sorted, de-duplicated).
    let mut branch_names: BTreeSet<BString> = BTreeSet::new();
    if let Some(sections) = file.sections_by_name("branch") {
        for section in sections {
            if let Some(sub) = section.header().subsection_name() {
                branch_names.insert(sub.to_owned());
            }
        }
    }

    let mut out = Vec::new();
    for bn in branch_names {
        let remote = match file.string_by("branch", Some(bn.as_bstr()), "remote") {
            Some(r) => r,
            None => continue,
        };
        if remote.to_str_lossy() != name {
            continue;
        }
        let merges = match file.strings_by("branch", Some(bn.as_bstr()), "merge") {
            Some(m) if !m.is_empty() => m,
            _ => continue,
        };
        let rebase = branch_rebase_modes(repo)
            .get(&bn)
            .copied()
            .unwrap_or(Some(super::pull::RebaseMode::Disabled));

        let merges: Vec<String> = merges
            .iter()
            .map(|m| {
                let s = m.to_str_lossy();
                s.strip_prefix("refs/heads/").unwrap_or(&s).to_string()
            })
            .collect();

        out.push((bn.to_str_lossy().into_owned(), rebase, merges));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

// ---------------------------------------------------------------------------
// remote prune / update
// ---------------------------------------------------------------------------

/// `git remote prune [-n] <name>` — delete the tracking refs whose remote-side
/// branch is gone. The remote is contacted (read-only) to learn which refs it
/// still advertises.
fn prune(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut dry_run = false;
    let mut pos: Vec<&str> = Vec::new();
    for a in args {
        if let Some(code) = help_requested(a, "n", USAGE_PRUNE) {
            return Ok(code);
        }
        let resolved = match canonical(a, OPTS_PRUNE, USAGE_PRUNE) {
            Ok(name) => name,
            Err(code) => return Ok(code),
        };
        match resolved.as_ref() {
            "-n" | "--dry-run" => dry_run = true,
            "--no-dry-run" => dry_run = false,
            s if s.starts_with('-') && s.len() > 1 => {
                unknown_option(s);
                return usage(USAGE_PRUNE);
            }
            _ => pos.push(a.as_str()),
        }
    }
    // `if (argc < 1) usage_with_options(...)`, then `prune_remote()` for each name in
    // turn (builtin/remote.c:1681-1685); a remote that cannot be reached dies there.
    if pos.is_empty() {
        return usage(USAGE_PRUNE);
    }
    for name in pos {
        if !prune_one(repo, name, dry_run)? {
            return Ok(ExitCode::from(128));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Prune `<name>`, returning `false` when the remote could not be contacted
/// (the `fatal:` line has already been printed in that case).
fn prune_one(repo: &gix::Repository, name: &str, dry_run: bool) -> Result<bool> {
    // `prune_remote()` → `get_remote_ref_states()` → `remote_get()`
    // (builtin/remote.c:1632, 1151).
    read_config(repo);
    let stale = match stale_tracking_refs(repo, name) {
        Ok(stale) => stale,
        Err(e) => {
            if crate::transport_err::file_url_fatal_in(&e).is_none() {
                eprintln!("fatal: {e}");
            }
            return Ok(false);
        }
    };

    // git's `prune_remote` returns before printing anything when the remote has
    // nothing stale, so a no-op prune is silent.
    if stale.is_empty() {
        return Ok(true);
    }

    println!("Pruning {name}");
    println!(
        "URL: {}",
        fetch_urls_or_name(repo, name)
            .first()
            .map_or(name, String::as_str)
    );

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    for full in &stale {
        let short = full.strip_prefix("refs/remotes/").unwrap_or(full);
        if dry_run {
            println!(" * [would prune] {short}");
        } else {
            println!(" * [pruned] {short}");
            delete_ref(repo, full_name(full)?)?;
        }
    }
    Ok(true)
}

/// Connect to `<name>` (fetch direction) and run the ref-advertisement
/// handshake, returning the resulting ref map. Contacting the remote is the
/// only way to learn what it advertises, so this fails when it is unreachable.
///
/// `remote.<name>.uploadpack` names the program run in place of
/// `git-upload-pack` on the other end, exactly as git's `transport_get()`
/// copies `remote->uploadpack` into `smart_options.uploadpack`. Only the
/// transports that spawn the service themselves (a local path and ssh) can
/// honour it, which is where git applies it too — so a bogus value makes
/// `remote show` and `remote prune` fail to reach a local remote, as it does
/// under stock git.
fn query_ref_map(repo: &gix::Repository, name: &str) -> Result<gix::remote::fetch::RefMap> {
    // `remote_get()` never fails for a well-formed name: one nothing configures is
    // made up with the name itself as its URL (`remotes_remote_get_1()`, remote.c:823-824),
    // so contacting it is contacting that path.
    let remote = match repo.find_remote(name) {
        Err(gix::remote::find::existing::Error::NotFound { .. }) => repo.remote_at(name)?,
        found => found?,
    };
    let upload_pack = repo
        .config_snapshot()
        .plumbing()
        .string_by("remote", Some(BStr::new(name)), "uploadpack");
    let connection = remote.connect_with_options(
        gix::remote::Direction::Fetch,
        // `git remote` has no `--ipv4`/`--ipv6`.
        gix::remote::connect::Options {
            upload_pack: super::fetch::local_service_program(
                remote.url(gix::remote::Direction::Fetch),
                upload_pack,
                "upload-pack",
            ),
            // git has moved to the top of the work tree by now; a relative URL is read from there.
            current_dir: crate::setup::setup_cwd(repo),
            ..Default::default()
        },
    )?;
    // `transport_get_remote_refs()` asks for the *whole* advertisement: `show`
    // needs `HEAD` to report the remote's HEAD branch, and the refspec-derived
    // `ref-prefix` filter would hide it (the fetch refspec only names
    // `refs/heads/`). git sends no prefixes at all here.
    let (map, _handshake) = connection.ref_map(
        gix::progress::Discard,
        gix::remote::ref_map::Options {
            prefix_from_spec_as_filter_on_remote: false,
            ..Default::default()
        },
    )?;
    Ok(map)
}

/// Port of git's `get_head_names` + `guess_remote_head` (invoked with
/// `REMOTE_GUESS_HEAD_ALL`): the abbreviated branch name(s) the remote's HEAD
/// resolves to, given the advertised refs.
///
/// When the advertisement carries a symbolic HEAD (the common case for both
/// protocol v1's `symref` capability and v2's `ls-refs`), git peeks directly at
/// its target and returns that single branch; otherwise it collects every
/// `refs/heads/*` whose object matches HEAD's, which may be several.
pub(super) fn remote_head_names(map: &gix::remote::fetch::RefMap) -> Vec<String> {
    use gix::protocol::handshake::Ref;

    let Some(head) = map.remote_refs.iter().find(|r| r.unpack().0 == "HEAD") else {
        return Vec::new();
    };

    // Advertised branch heads: (full name, ultimate object it points to).
    let heads: Vec<(BString, gix::hash::ObjectId)> = map
        .remote_refs
        .iter()
        .filter_map(|r| {
            let (name, target, peeled) = r.unpack();
            let oid = peeled.or(target)?;
            name.starts_with_str("refs/heads/")
                .then(|| (name.to_owned(), oid.to_owned()))
        })
        .collect();

    // Transport peeked at where HEAD points: use it directly, if advertised.
    if let Ref::Symbolic { target, .. } | Ref::Unborn { target, .. } = head {
        return if heads.iter().any(|(n, _)| n == target) {
            vec![abbrev_branch(target.as_bstr())]
        } else {
            Vec::new()
        };
    }

    // No symref: match every head that points at the same object as HEAD.
    let (_, head_target, head_peeled) = head.unpack();
    let Some(head_oid) = head_peeled.or(head_target) else {
        return Vec::new();
    };
    heads
        .iter()
        .filter(|(_, oid)| **oid == *head_oid)
        .map(|(n, _)| abbrev_branch(n.as_bstr()))
        .collect()
}

/// git's `abbrev_branch`: strip a leading `refs/heads/`, otherwise return the
/// name unchanged (so remote-tracking refs keep their full path).
fn abbrev_branch(name: &BStr) -> String {
    let s = name.to_str_lossy();
    s.strip_prefix("refs/heads/").unwrap_or(&s).to_string()
}

/// The current on-disk value of `refs/remotes/<name>/HEAD` before an update, as
/// git's `refs_update_symref_extended` reports it: `Some(symref target)` /
/// `Some(hex oid)` with the bool marking a detached (direct object) ref, or
/// `None` when the ref does not yet exist.
pub(super) fn symref_prev(repo: &gix::Repository, head: &str) -> Result<(Option<String>, bool)> {
    match repo.try_find_reference(head)? {
        None => Ok((None, false)),
        Some(reference) => match &reference.inner.target {
            Target::Symbolic(target) => {
                Ok((Some(target.as_bstr().to_str_lossy().into_owned()), false))
            }
            Target::Object(id) => Ok((Some(id.to_hex().to_string()), true)),
        },
    }
}

/// Port of git's `report_set_head_auto`: the line printed after `set-head -a`
/// succeeds, describing how `refs/remotes/<remote>/HEAD` changed.
fn report_set_head_auto(remote: &str, head_name: &str, prev: &Option<String>, was_detached: bool) {
    let prefix = format!("refs/remotes/{remote}/");
    match prev {
        Some(local) => {
            if let Some(prev_head) = local.strip_prefix(&prefix) {
                if prev_head == head_name {
                    println!("'{remote}/HEAD' is unchanged and points to '{head_name}'");
                } else {
                    println!(
                        "'{remote}/HEAD' has changed from '{prev_head}' and now points to '{head_name}'"
                    );
                }
            } else if was_detached {
                println!(
                    "'{remote}/HEAD' was detached at '{local}' and now points to '{head_name}'"
                );
            } else {
                println!(
                    "'{remote}/HEAD' used to point to '{local}' (which is not a remote branch), but now points to '{head_name}'"
                );
            }
        }
        None => println!("'{remote}/HEAD' is now created and points to '{head_name}'"),
    }
}

/// Tracking refs of `<name>` that the remote no longer advertises, as full ref
/// names. Contacting the remote is the only way to know, so this fails when the
/// remote is unreachable.
fn stale_tracking_refs(repo: &gix::Repository, name: &str) -> Result<Vec<String>> {
    let map = query_ref_map(repo, name)?;

    let live: BTreeSet<String> = map
        .mappings
        .iter()
        .filter_map(|m| m.local.as_ref())
        .map(|l| l.to_str_lossy().into_owned())
        .collect();

    // ```c
    // memset(&query, 0, sizeof(struct refspec_item));
    // query.dst = (char *)refname;
    //
    // query_refspecs_multiple(info->rs, &query, &matches);
    // if (matches.nr == 0)
    //         goto clean_exit; /* No matches */
    // ```
    //
    // (`get_stale_heads_cb()`, remote.c.) The candidates are the refs the *refspec
    // destinations* cover, not the ones under `refs/remotes/<name>/`: a remote configured
    // with `+refs/heads/nosuch/*:refs/remotes/origin/*` prunes under `refs/remotes/origin/`,
    // where its own name never appears.
    let dsts = fetch_refspec_dsts(repo, name);
    let mut stale = Vec::new();
    let platform = repo.references()?;
    for dst in &dsts {
        let prefix = match dst.split_once('*') {
            Some((head, _)) => head.to_string(),
            None => dst.clone(),
        };
        for reference in platform.prefixed(prefix.as_bytes())? {
            let reference = reference.map_err(|e| anyhow::anyhow!("{e}"))?;
            let full = reference.name().as_bstr().to_str_lossy().into_owned();
            // The tracking `HEAD` is a symref into the namespace rather than a branch of
            // its own, and git leaves it alone.
            if full.ends_with("/HEAD") {
                continue;
            }
            if live.contains(&full) || stale.contains(&full) {
                continue;
            }
            if !refspec_dst_matches(dst, &full) {
                continue;
            }
            stale.push(full);
        }
    }
    stale.sort();
    Ok(stale)
}

/// `git remote update [-p] [<group>|<remote>]…` — fetch from every named
/// remote (all of them by default).
///
/// ```c
/// strvec_push(&cmd.args, "fetch");
///
/// if (prune != -1)
///         strvec_push(&cmd.args, prune ? "--prune" : "--no-prune");
/// if (verbose)
///         strvec_push(&cmd.args, "-v");
/// strvec_push(&cmd.args, "--multiple");
/// if (argc < 2)
///         strvec_push(&cmd.args, "default");
/// for (i = 1; i < argc; i++)
///         strvec_push(&cmd.args, argv[i]);
///
/// if (strcmp(cmd.args.v[cmd.args.nr-1], "default") == 0) {
///         repo_config(the_repository, get_remote_default, &default_defined);
///         if (!default_defined) {
///                 strvec_pop(&cmd.args);
///                 strvec_push(&cmd.args, "--all");
///         }
/// }
///
/// cmd.git_cmd = 1;
/// return run_command(&cmd);
/// ```
///
/// (`update()`, builtin/remote.c:1701-1739.) Everything past the command line —
/// which remotes a group or `--all` names, `skipFetchAll`, the `Fetching <name>`
/// lines, the refusals — belongs to that `git fetch` child, and so does the exit:
/// a child that dies with 128 is a non-zero `run_command()`, which `cmd_remote()`'s
/// `return !!fn(argc, argv, prefix, repo)` (:1959) reports as 1. Calling the fetch
/// in this process instead let its `die()` end `git remote` itself with 128.
fn update(repo: &gix::Repository, args: &[String], verbose: bool) -> Result<ExitCode> {
    let mut prune: Option<bool> = None;
    let mut operands: Vec<String> = Vec::new();
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        // `parse_options()` stops at `--` and `--end-of-options`: what follows is an
        // operand even when it looks like an option, and reaches `git fetch` as one.
        if a == "--" || a == "--end-of-options" {
            operands.extend(rest.cloned());
            break;
        }
        if let Some(code) = help_requested(a, "p", USAGE_UPDATE) {
            return Ok(code);
        }
        let resolved = match canonical(a, OPTS_UPDATE, USAGE_UPDATE) {
            Ok(name) => name,
            Err(code) => return Ok(code),
        };
        match resolved.as_ref() {
            "-p" | "--prune" => prune = Some(true),
            "--no-prune" => prune = Some(false),
            s if s.starts_with('-') && s.len() > 1 => {
                unknown_option(s);
                return usage(USAGE_UPDATE);
            }
            _ => operands.push(a.clone()),
        }
    }

    let mut cmd: Vec<String> = vec!["fetch".to_string()];
    if let Some(prune) = prune {
        cmd.push(if prune { "--prune" } else { "--no-prune" }.to_string());
    }
    if verbose {
        cmd.push("-v".to_string());
    }
    cmd.push("--multiple".to_string());
    if operands.is_empty() {
        cmd.push("default".to_string());
    }
    cmd.extend(operands);

    if cmd.last().is_some_and(|last| last == "default") {
        // `get_remote_default()` (builtin/remote.c:1690-1699): any `remotes.default`.
        let default_defined = repo
            .config_snapshot()
            .plumbing()
            .strings_by("remotes", None::<&BStr>, "default")
            .is_some();
        if !default_defined {
            cmd.pop();
            cmd.push("--all".to_string());
        }
    }

    let code = super::fetch::run_git_cmd(&cmd);
    Ok(ExitCode::from(u8::from(code != 0)))
}

/// git's `remote->skip_default_update` for `<name>`: the effective value of
/// `remote.<name>.skipFetchAll`, whose deprecated synonym
/// `remote.<name>.skipDefaultUpdate` is parsed into the very same field. Because
/// both names write one slot as the config is read in order, the *last*
/// occurrence of either wins — `skipFetchAll = true` followed by
/// `skipDefaultUpdate = false` leaves the remote unskipped.
///
/// The ordered walk is what makes that rule expressible: the merged config file
/// is visited section by section (system, then global, then local, matching
/// git's parse order) and, inside each, value name by value name. A key written
/// without `=` counts as `true`, as `git_config_bool` treats a `NULL` value, and
/// an unparseable value leaves the previous verdict alone.
pub(super) fn skip_fetch_all(repo: &gix::Repository, name: &str) -> bool {
    /// git's `git_parse_maybe_bool`, which is deliberately narrower than a
    /// number parse: anything else is a config error, which git reports and this
    /// caller ignores in favour of the value already decided.
    fn parse_bool(value: &BStr) -> Option<bool> {
        match value.to_str_lossy().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Some(true),
            "false" | "no" | "off" | "0" | "" => Some(false),
            _ => None,
        }
    }

    let cfg = repo.config_snapshot();
    let Some(sections) = cfg.plumbing().sections_by_name("remote") else {
        return false;
    };

    let mut skip = false;
    for section in sections {
        if section.header().subsection_name() != Some(BStr::new(name)) {
            continue;
        }
        let body = section.body();

        // The last of the two synonyms in this section, as (config name, which
        // occurrence of that name it is).
        let mut last: Option<(&str, usize)> = None;
        let (mut fetch_all, mut default_update) = (0usize, 0usize);
        for value_name in body.value_names() {
            match value_name.to_ascii_lowercase().as_str() {
                "skipfetchall" => {
                    last = Some(("skipFetchAll", fetch_all));
                    fetch_all += 1;
                }
                "skipdefaultupdate" => {
                    last = Some(("skipDefaultUpdate", default_update));
                    default_update += 1;
                }
                _ => {}
            }
        }

        if let Some((key, nth)) = last {
            skip = match body.values(key).get(nth) {
                Some(value) => parse_bool(value.as_bstr()).unwrap_or(skip),
                // The name was there but carried no `= value`.
                None => true,
            };
        }
    }
    skip
}

/// Move one ref's reflog from `old_full` to `new_full`, the way
/// `files_copy_or_rename_ref()` renames a ref's log along with the ref.
///
/// A ref with no log needs nothing moved, and a destination that somehow has one
/// already is left alone rather than clobbered — the caller has refused an
/// occupied destination ref before it gets here.
fn move_reflog(repo: &gix::Repository, old_full: &str, new_full: &str) -> Result<()> {
    let logs = repo.git_dir().join("logs");
    let from = logs.join(old_full);
    let to = logs.join(new_full);
    if !from.is_file() || to.exists() {
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&from, &to)?;
    // The directory the log came out of is git's to remove once it is empty.
    let mut dir = from.parent().map(std::path::Path::to_path_buf);
    while let Some(current) = dir {
        if current == logs || std::fs::remove_dir(&current).is_err() {
            break;
        }
        dir = current.parent().map(std::path::Path::to_path_buf);
    }
    Ok(())
}
