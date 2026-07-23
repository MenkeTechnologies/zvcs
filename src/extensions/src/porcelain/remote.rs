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
///   * Creating `refs/remotes/<name>/HEAD` writes no reflog entry (gitoxide
///     skips reflogs for symbolic-target updates); stock `set-head` writes one.
pub fn remote(args: &[String]) -> Result<ExitCode> {
    let mut verbose = false;
    let mut idx = 0;
    while let Some(a) = args.get(idx).map(String::as_str) {
        match a {
            "-v" | "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            "-h" | "--help" => {
                print!("{}", USAGE_MAIN);
                return Ok(ExitCode::from(129));
            }
            _ if a.starts_with('-') => {
                unknown_option(a);
                return usage(USAGE_MAIN);
            }
            _ => break,
        }
        idx += 1;
    }
    let rest = &args[idx..];

    let repo = match gix::discover(".") {
        Ok(repo) => repo,
        Err(_) => {
            return fatal("not a git repository (or any of the parent directories): .git");
        }
    };

    match rest.first().map(String::as_str) {
        None => list(&repo, verbose),
        Some("add") => add(&repo, &rest[1..]),
        Some("rename") => rename(&repo, &rest[1..]),
        Some("remove") | Some("rm") => remove(&repo, &rest[1..]),
        Some("set-head") => set_head(&repo, &rest[1..]),
        Some("set-branches") => set_branches(&repo, &rest[1..]),
        Some("get-url") => get_url(&repo, &rest[1..]),
        Some("set-url") => set_url(&repo, &rest[1..]),
        Some("show") => show(&repo, &rest[1..], verbose),
        Some("prune") => prune(&repo, &rest[1..]),
        Some("update") => update(&repo, &rest[1..]),
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
fn remote_exists(repo: &gix::Repository, name: &str) -> bool {
    repo.remote_names().iter().any(|n| n.to_str_lossy() == name)
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

/// The fetch URL list git would use, which falls back to the remote's own name
/// when nothing is configured.
fn fetch_urls_or_name(repo: &gix::Repository, name: &str) -> Vec<String> {
    let urls = effective_urls(repo, name, "url");
    if urls.is_empty() {
        vec![name.to_string()]
    } else {
        urls
    }
}

/// The push URL list: explicit `pushurl` entries, else the fetch URLs.
fn push_urls_or_name(repo: &gix::Repository, name: &str) -> Vec<String> {
    let push = effective_urls(repo, name, "pushurl");
    if push.is_empty() {
        fetch_urls_or_name(repo, name)
    } else {
        push
    }
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
fn tracking_refs(repo: &gix::Repository, name: &str) -> Result<Vec<(FullName, Target)>> {
    let prefix = format!("refs/remotes/{name}/");
    let platform = repo.references()?;

    let mut out = Vec::new();
    for reference in platform.prefixed(prefix.as_bytes())? {
        let reference = reference.map_err(|e| anyhow::anyhow!("{e}"))?;
        out.push((reference.name().to_owned(), reference.inner.target.clone()));
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

/// Raw multi-values of `remote.<name>.<key>` across all config scopes.
fn effective_specs(repo: &gix::Repository, name: &str, key: &str) -> Vec<String> {
    let cfg = repo.config_snapshot();
    cfg.plumbing()
        .strings_by("remote", Some(BStr::new(name)), key)
        .map(|values| {
            values
                .into_iter()
                .map(|v| v.to_str_lossy().into_owned())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default()
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
    for name in repo.remote_names() {
        let name = name.to_str_lossy();
        if !verbose {
            println!("{name}");
            continue;
        }
        let fetch = effective_urls(repo, &name, "url");
        let push = effective_urls(repo, &name, "pushurl");
        match fetch.first() {
            Some(url) => println!("{name}\t{url} (fetch)"),
            None => println!("{name}\t"),
        }
        if push.is_empty() {
            if let Some(url) = fetch.first() {
                println!("{name}\t{url} (push)");
            }
        } else {
            for url in &push {
                println!("{name}\t{url} (push)");
            }
        }
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
    while let Some(a) = args.get(i).map(String::as_str) {
        i += 1;
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
            _ if a.starts_with("--mirror=") => {
                eprintln!(
                    "error: unknown --mirror argument: {}",
                    &a["--mirror=".len()..]
                );
                return usage(USAGE_ADD);
            }
            _ if a.starts_with("--track=") => tracks.push(a["--track=".len()..].to_string()),
            _ if a.starts_with("--master=") => master = Some(a["--master=".len()..].to_string()),
            _ if a.starts_with("-t") && a.len() > 2 => tracks.push(a[2..].to_string()),
            _ if a.starts_with("-m") && a.len() > 2 => master = Some(a[2..].to_string()),
            _ if a.starts_with('-') && a.len() > 1 => {
                unknown_option(a);
                return usage(USAGE_ADD);
            }
            _ => pos.push(a),
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

    if !valid_remote_name(name) {
        return fatal(format!("'{name}' is not a valid remote name"));
    }
    if !tracks.is_empty() && mirror == Mirror::Push {
        return fatal("specifying branches to track makes sense only with fetch mirrors");
    }
    if remote_exists(repo, name) {
        return error(format!("remote {name} already exists."), 3);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let (path, mut file) = open_local(repo)?;
    {
        let mut section = file.section_mut_or_create_new("remote", Some(BStr::new(name)))?;
        section.push("url", url)?;
        if mirror.fetches() {
            section.push("fetch", "+refs/*:refs/*")?;
        } else if tracks.is_empty() {
            let spec = format!("+refs/heads/*:refs/remotes/{name}/*");
            section.push("fetch", spec.as_str())?;
        } else {
            for branch in &tracks {
                let spec = format!("+refs/heads/{branch}:refs/remotes/{name}/{branch}");
                section.push("fetch", spec.as_str())?;
            }
        }
        if let Some(t) = tag_opt {
            section.push("tagOpt", t)?;
        }
        if mirror.pushes() {
            section.push("mirror", "true")?;
        }
    }
    persist(&path, &file)?;

    if let Some(branch) = &master {
        let head = full_name(&format!("refs/remotes/{name}/HEAD"))?;
        let target = full_name(&format!("refs/remotes/{name}/{branch}"))?;
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: format!("remote add {name}").into(),
                },
                expected: PreviousValue::Any,
                new: Target::Symbolic(target),
            },
            name: head,
            deref: false,
        })?;
    }

    if do_fetch {
        eprintln!("Updating {name}");
        if super::fetch::fetch(&[name.to_string()]).is_err() {
            eprintln!("error: Could not fetch {name}");
            return Ok(ExitCode::from(1));
        }
    }

    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// remote rename
// ---------------------------------------------------------------------------

/// `git remote rename <old> <new>` — move the config section, the tracking
/// refs and every `branch.*` / `remote.pushDefault` back-reference.
///
/// The ordering mirrors stock git: config first, then the refs, then the fetch
/// refspecs. A ref collision therefore aborts with the section already renamed
/// and the refspecs untouched, exactly as git leaves it.
fn rename(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut pos: Vec<&str> = Vec::new();
    for a in args {
        match a.as_str() {
            "--progress" | "--no-progress" => {}
            s if s.starts_with('-') && s.len() > 1 => {
                unknown_option(s);
                return usage(USAGE_RENAME);
            }
            s => pos.push(s),
        }
    }
    if pos.len() != 2 {
        return usage(USAGE_RENAME);
    }
    let (old, new) = (pos[0], pos[1]);

    if !remote_exists(repo, old) {
        return error(format!("No such remote: '{old}'"), 2);
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
    // A remote configured outside the repository has nothing local to rename;
    // the back-references below still have to be rewritten.
    let _ = file.rename_section("remote", Some(BStr::new(old)), "remote", new.to_string());

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
    if let Ok(mut section) = file.section_mut("remote", None::<&BStr>) {
        if section
            .value("pushDefault")
            .map(|v| v == old)
            .unwrap_or(false)
        {
            section.set("pushDefault", new)?;
        }
    }
    persist(&path, &file)?;

    // 2. refs: refuse before touching anything if a destination is occupied.
    let refs = tracking_refs(repo, old)?;
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

    // Carry the reflogs across by moving the directory, so history survives the
    // create-then-delete below.
    let logs = repo.git_dir().join("logs").join("refs").join("remotes");
    let (from, to) = (logs.join(old), logs.join(new));
    if from.exists() && !to.exists() {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&from, &to)?;
    }

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
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: format!("remote: renamed {old_text} to {dst}").into(),
                },
                expected: PreviousValue::Any,
                new: new_target,
            },
            name: new_name,
            deref: false,
        })?;
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

// ---------------------------------------------------------------------------
// remote remove
// ---------------------------------------------------------------------------

/// `git remote remove <name>` — drop the config section, every tracking ref,
/// and the `branch.*` / `remote.pushDefault` settings that named it.
fn remove(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut pos: Vec<&str> = Vec::new();
    for a in args {
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

    if !remote_exists(repo, name) {
        return error(format!("No such remote: '{name}'"), 2);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    for (ref_name, _) in tracking_refs(repo, name)? {
        delete_ref(repo, ref_name)?;
    }

    let (path, mut file) = open_local(repo)?;
    while file
        .remove_section("remote", Some(BStr::new(name)))
        .is_some()
    {}

    for branch in branch_subsections(&file) {
        let mut now_empty = false;
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

    let mut drop_remote_section = false;
    if let Ok(mut section) = file.section_mut("remote", None::<&BStr>) {
        if section
            .value("pushDefault")
            .map(|v| v == name)
            .unwrap_or(false)
        {
            while section.remove("pushDefault").is_some() {}
            drop_remote_section = section.num_values() == 0;
        }
    }
    if drop_remote_section {
        let _ = file.remove_section("remote", None::<&BStr>);
    }
    persist(&path, &file)?;

    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// remote set-head / set-branches
// ---------------------------------------------------------------------------

/// `git remote set-head <name> (-a | -d | <branch>)` — point (or drop) the
/// `refs/remotes/<name>/HEAD` symref. `-a` contacts the remote and derives the
/// branch from its advertised HEAD.
fn set_head(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut auto = false;
    let mut delete = false;
    let mut pos: Vec<&str> = Vec::new();
    for a in args {
        match a.as_str() {
            "-a" | "--auto" => auto = true,
            "-d" | "--delete" => delete = true,
            s if s.starts_with('-') && s.len() > 1 => {
                unknown_option(s);
                return usage(USAGE_SET_HEAD);
            }
            s => pos.push(s),
        }
    }
    if pos.is_empty() || (!auto && !delete && pos.len() != 2) {
        return usage(USAGE_SET_HEAD);
    }
    let name = pos[0];
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
            Err(e) => return fatal(e),
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
        report_set_head_auto(name, head_name, &prev, was_detached);
        return Ok(ExitCode::SUCCESS);
    }

    let branch = pos[1];
    let target = format!("refs/remotes/{name}/{branch}");
    if repo.try_find_reference(target.as_str())?.is_none() {
        return error(format!("Not a valid ref: {target}"), 1);
    }
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
    Ok(ExitCode::SUCCESS)
}

/// `git remote set-branches [--add] <name> <branch>…` — rewrite (or extend)
/// the fetch refspecs so only the named branches are tracked.
fn set_branches(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut append = false;
    let mut pos: Vec<&str> = Vec::new();
    for a in args {
        match a.as_str() {
            "--add" => append = true,
            "--no-add" => append = false,
            s if s.starts_with('-') && s.len() > 1 => {
                unknown_option(s);
                return usage(USAGE_SET_BRANCHES);
            }
            s => pos.push(s),
        }
    }
    let Some(name) = pos.first().copied() else {
        eprintln!("error: no remote specified");
        return usage(USAGE_SET_BRANCHES);
    };
    if !remote_exists(repo, name) {
        return error(format!("No such remote '{name}'"), 2);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let (path, mut file) = open_local(repo)?;
    {
        let mut section = file.section_mut_or_create_new("remote", Some(BStr::new(name)))?;
        if !append {
            while section.remove("fetch").is_some() {}
        }
        for branch in &pos[1..] {
            let spec = format!("+refs/heads/{branch}:refs/remotes/{name}/{branch}");
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
        match a.as_str() {
            "--push" => push = true,
            "--no-push" => push = false,
            "--all" => all = true,
            "--no-all" => all = false,
            s if s.starts_with('-') && s.len() > 1 => {
                unknown_option(s);
                return usage(USAGE_GET_URL);
            }
            s => pos.push(s),
        }
    }
    if pos.len() != 1 {
        return usage(USAGE_GET_URL);
    }
    let name = pos[0];
    if !remote_exists(repo, name) {
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
        match a.as_str() {
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
            s => pos.push(s),
        }
    }
    if append && delete {
        return usage(USAGE_SET_URL);
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
    if !remote_exists(repo, name) {
        return error(format!("No such remote '{name}'"), 2);
    }

    let key = if push { "pushurl" } else { "url" };

    // git compiles the --delete argument as an extended regular expression.
    let delete_re = if delete {
        match regex::bytes::RegexBuilder::new(value).unicode(false).build() {
            Ok(re) => Some(re),
            Err(_) => return fatal(format!("Invalid old URL pattern: {value}")),
        }
    } else {
        None
    };

    // Deleting every non-push URL would leave the remote unusable; git refuses
    // when the pattern matches all of them (no URL fails to match).
    if let Some(re) = delete_re.as_ref() {
        if !push {
            let survives = effective_urls(repo, name, "url")
                .into_iter()
                .filter(|u| !re.is_match(u.as_bytes()))
                .count();
            if survives == 0 {
                return fatal("Will not delete all non-push URLs");
            }
        }
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let (path, mut file) = open_local(repo)?;
    {
        let mut section = file.section_mut_or_create_new("remote", Some(BStr::new(name)))?;
        let current: Vec<String> = section
            .values(key)
            .into_iter()
            .map(|v| v.to_str_lossy().into_owned())
            .collect();

        let updated: Vec<String> = if append {
            let mut v = current;
            v.push(value.to_string());
            v
        } else if let Some(re) = delete_re.as_ref() {
            current
                .into_iter()
                .filter(|u| !re.is_match(u.as_bytes()))
                .collect()
        } else if let Some(old) = pos.get(2) {
            current
                .into_iter()
                .map(|u| {
                    if u.as_str() == *old {
                        value.to_string()
                    } else {
                        u
                    }
                })
                .collect()
        } else if current.is_empty() {
            vec![value.to_string()]
        } else {
            // Replace the last configured URL, leaving any earlier ones alone.
            let mut v = current;
            let last = v.len() - 1;
            v[last] = value.to_string();
            v
        };

        while section.remove(key).is_some() {}
        for url in &updated {
            section.push(key, url.as_str())?;
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
        match a.as_str() {
            "-n" | "--no-query" => no_query = true,
            s if s.starts_with('-') && s.len() > 1 => {
                unknown_option(s);
                return usage(USAGE_SHOW);
            }
            s => names.push(s),
        }
    }
    if names.is_empty() {
        return list(repo, verbose);
    }
    for name in &names {
        // Without `-n`, contact the remote once; a failure is fatal, exactly as
        // git's `transport_get_remote_refs` dies.
        let map = if no_query {
            None
        } else {
            match query_ref_map(repo, name) {
                Ok(map) => Some(map),
                Err(e) => {
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
        let any_rebase = pulls.iter().any(|(_, rebase, _)| *rebase);

        for (bname, rebase, merges) in &pulls {
            let first = merges.first().map(String::as_str).unwrap_or("");
            if *rebase {
                // rebase requires a single merge ref; git errors on more.
                println!("    {bname:<width$} rebases onto remote {first}");
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

    show_push(repo, name);
    Ok(())
}

/// The `'git push'` section of a `show` block: mirror note, explicit refspecs,
/// or the default matching behaviour.
fn show_push(repo: &gix::Repository, name: &str) {
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
    if specs.is_empty() {
        println!("  Local ref configured for 'git push' (status not queried):");
        println!("    (matching) pushes to (matching)");
        return;
    }

    // (source ref, displayed source, verb, destination), ordered by the source
    // ref — a deletion refspec has an empty source and therefore sorts first.
    let mut rows: Vec<(String, String, &str, String)> = specs
        .iter()
        .map(|spec| {
            let forced = spec.starts_with('+');
            let body = spec.strip_prefix('+').unwrap_or(spec);
            let (src, dst) = match body.split_once(':') {
                Some((s, d)) => (s.to_string(), d.to_string()),
                None => (body.to_string(), body.to_string()),
            };
            let shown = if src.is_empty() {
                "(delete)".to_string()
            } else {
                src.clone()
            };
            let verb = if forced { "forces to" } else { "pushes to" };
            (src, shown, verb, dst)
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));

    if rows.len() == 1 {
        println!("  Local ref configured for 'git push' (status not queried):");
    } else {
        println!("  Local refs configured for 'git push' (status not queried):");
    }
    let width = rows
        .iter()
        .map(|(_, shown, _, _)| shown.chars().count())
        .max()
        .unwrap_or(0);
    for (_, shown, verb, dst) in &rows {
        println!("    {shown:<width$} {verb} {dst}");
    }
}

/// Sorted short names of the remote-tracking branches for `<name>`: the refs
/// under `refs/remotes/<name>/` that the fetch refspecs select, with the
/// `<name>/` prefix stripped and the `HEAD` symref excluded.
fn remote_branches(repo: &gix::Repository, name: &str) -> Result<Vec<String>> {
    let dsts = fetch_refspec_dsts(repo, name);
    let prefix = format!("refs/remotes/{name}/");

    let mut out = Vec::new();
    for (ref_name, _) in tracking_refs(repo, name)? {
        let full = ref_name.as_bstr().to_str_lossy().into_owned();
        let Some(branch) = full.strip_prefix(&prefix) else {
            continue;
        };
        if branch == "HEAD" {
            continue;
        }
        if !dsts.iter().any(|d| refspec_dst_matches(d, &full)) {
            continue;
        }
        out.push(branch.to_string());
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
    for m in &map.mappings {
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
fn pull_config(repo: &gix::Repository, name: &str) -> Result<Vec<(String, bool, Vec<String>)>> {
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
        let rebase = file
            .boolean_by("branch", Some(bn.as_bstr()), "rebase")
            .ok()
            .flatten()
            .unwrap_or(false);

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
        match a.as_str() {
            "-n" | "--dry-run" => dry_run = true,
            s if s.starts_with('-') && s.len() > 1 => {
                unknown_option(s);
                return usage(USAGE_PRUNE);
            }
            s => pos.push(s),
        }
    }
    if pos.len() != 1 {
        return usage(USAGE_PRUNE);
    }
    match prune_one(repo, pos[0], dry_run)? {
        true => Ok(ExitCode::SUCCESS),
        false => Ok(ExitCode::from(128)),
    }
}

/// Prune `<name>`, returning `false` when the remote could not be contacted
/// (the `fatal:` line has already been printed in that case).
fn prune_one(repo: &gix::Repository, name: &str, dry_run: bool) -> Result<bool> {
    let stale = match stale_tracking_refs(repo, name) {
        Ok(stale) => stale,
        Err(e) => {
            eprintln!("fatal: {e}");
            return Ok(false);
        }
    };

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
fn query_ref_map(repo: &gix::Repository, name: &str) -> Result<gix::remote::fetch::RefMap> {
    let remote = repo.find_remote(name)?;
    let connection = remote.connect(gix::remote::Direction::Fetch)?;
    let (map, _handshake) = connection.ref_map(
        gix::progress::Discard,
        gix::remote::ref_map::Options::default(),
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
fn remote_head_names(map: &gix::remote::fetch::RefMap) -> Vec<String> {
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
fn symref_prev(repo: &gix::Repository, head: &str) -> Result<(Option<String>, bool)> {
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

    let dsts = fetch_refspec_dsts(repo, name);
    let prefix = format!("refs/remotes/{name}/");
    let mut stale = Vec::new();
    for (ref_name, _) in tracking_refs(repo, name)? {
        let full = ref_name.as_bstr().to_str_lossy().into_owned();
        if !full.starts_with(&prefix) || full == format!("{prefix}HEAD") {
            continue;
        }
        if live.contains(&full) {
            continue;
        }
        if !dsts.iter().any(|d| refspec_dst_matches(d, &full)) {
            continue;
        }
        stale.push(full);
    }
    Ok(stale)
}

/// `git remote update [-p] [<group>|<remote>]…` — fetch from every named
/// remote (all of them by default), then optionally prune.
fn update(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut do_prune = false;
    let mut pos: Vec<&str> = Vec::new();
    for a in args {
        match a.as_str() {
            "-p" | "--prune" => do_prune = true,
            "--no-prune" => do_prune = false,
            s if s.starts_with('-') && s.len() > 1 => {
                unknown_option(s);
                return usage(USAGE_UPDATE);
            }
            s => pos.push(s),
        }
    }

    let known: Vec<String> = repo
        .remote_names()
        .iter()
        .map(|n| n.to_str_lossy().into_owned())
        .collect();

    let mut targets: Vec<String> = Vec::new();
    if pos.is_empty() {
        targets = known;
    } else {
        let cfg = repo.config_snapshot();
        for want in pos {
            if known.iter().any(|n| n.as_str() == want) {
                targets.push(want.to_string());
                continue;
            }
            let group = cfg
                .plumbing()
                .strings_by("remotes", None::<&BStr>, want)
                .unwrap_or_default();
            if group.is_empty() {
                eprintln!("fatal: no such remote or remote group: {want}");
                return Ok(ExitCode::from(1));
            }
            for entry in group {
                for member in entry.to_str_lossy().split_whitespace() {
                    targets.push(member.to_string());
                }
            }
        }
    }

    for name in &targets {
        eprintln!("Fetching {name}");
        if super::fetch::fetch(&[name.clone()]).is_err() {
            eprintln!("error: Could not fetch {name}");
            return Ok(ExitCode::from(1));
        }
        if do_prune && !prune_one(repo, name, false)? {
            return Ok(ExitCode::from(128));
        }
    }
    Ok(ExitCode::SUCCESS)
}
