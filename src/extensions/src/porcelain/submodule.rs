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

/// `git submodule` — inspect and register the submodules recorded in the index.
///
/// Two of the ten stock subcommands are ported here, both aiming at
/// byte-for-byte parity with stock `git`:
///
///   * `git submodule [--quiet] [--cached] [status] [--] [<path>...]`
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
/// `add`, `deinit`, `update`, `set-branch`, `set-url`, `summary`, `foreach`,
/// `sync`, `absorbgitdirs`, and `--recursive` on `status`. `init` additionally
/// bails on `.gitmodules` urls that are relative (`./`, `../`), which require
/// git's `resolve_relative_url` against the superproject's default remote.
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

    let (name, tail) = match args.get(i) {
        Some(a) if !a.starts_with('-') => (a.as_str(), &args[i + 1..]),
        // No subcommand (or the next token is an option): status is the default.
        _ => ("status", &args[i..]),
    };

    match name {
        "status" => status(tail, quiet, cached),
        "init" => init(tail, quiet),
        "add" => bail!("`submodule add` needs a clone of the new submodule; not ported"),
        "update" => bail!("`submodule update` needs clone/fetch/checkout of submodules; not ported"),
        "deinit" => bail!("`submodule deinit` removes submodule worktrees; not ported"),
        "sync" => bail!("`submodule sync` rewrites remote urls inside submodules; not ported"),
        "summary" => bail!("`submodule summary` needs the submodule log walk; not ported"),
        "foreach" => bail!("`submodule foreach` runs a shell command per submodule; not ported"),
        "set-branch" => bail!("`submodule set-branch` edits .gitmodules; not ported"),
        "set-url" => bail!("`submodule set-url` edits .gitmodules; not ported"),
        "absorbgitdirs" => bail!("`submodule absorbgitdirs` relocates git dirs; not ported"),
        other => bail!("unknown subcommand {other:?} (ported: status, init)"),
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

    for a in args {
        if no_more_opts {
            patterns.push(BString::from(a.as_str()));
            continue;
        }
        match a.as_str() {
            "--" => no_more_opts = true,
            "-q" | "--quiet" => quiet = true,
            "--cached" => cached = true,
            "--recursive" => bail!(
                "unsupported flag \"--recursive\": nested submodule status needs the super-prefix walk (ported: --quiet, --cached)"
            ),
            _ if a.starts_with('-') => {
                bail!("unsupported flag {a:?} (ported: --quiet, --cached)")
            }
            _ => patterns.push(BString::from(a.as_str())),
        }
    }

    let repo = gix::discover(".")?;
    let index = repo.open_index()?;

    let entries = match module_list(&repo, &index, &patterns)? {
        Ok(entries) => entries,
        // Unmatched pathspecs: git reports each one and exits 1.
        Err(code) => return Ok(code),
    };

    let submodules = submodules(&repo)?;
    let prefix = repo_prefix(&repo)?;
    let workdir = repo.workdir().map(ToOwned::to_owned);
    let null = ObjectId::null(repo.object_hash());

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    for entry in &entries {
        let Some(sub) = find_submodule(&submodules, &entry.path) else {
            out.flush()?;
            eprintln!(
                "fatal: no submodule mapping found in .gitmodules for path '{}'",
                entry.path
            );
            return Ok(ExitCode::from(128));
        };
        let display = display_path(entry.path.as_bstr(), prefix.as_ref());

        if entry.conflicted {
            print_status(&mut out, quiet, 'U', &null, &display, None)?;
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
            print_status(&mut out, quiet, '-', &entry.oid, &display, None)?;
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
        print_status(&mut out, quiet, state, &shown, &display, name.as_deref())?;
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
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
            _ if a.starts_with('-') => bail!("unsupported flag {a:?} (ported: --quiet)"),
            _ => patterns.push(BString::from(a.as_str())),
        }
    }

    let repo = gix::discover(".")?;
    let index = repo.open_index()?;

    let mut entries = match module_list(&repo, &index, &patterns)? {
        Ok(entries) => entries,
        Err(code) => return Ok(code),
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
) -> Result<std::result::Result<Vec<Entry>, ExitCode>> {
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
        return Ok(Err(ExitCode::from(1)));
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
