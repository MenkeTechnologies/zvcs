use anyhow::{anyhow, bail, Result};
use std::cmp::Ordering;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::objs::{CommitRef, Kind};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// The exact usage block stock git prints for an option-parsing error,
/// reproduced verbatim so `--list --show-current` and friends match byte for byte.
const USAGE: &str = r#"usage: git branch [<options>] [-r | -a] [--merged] [--no-merged]
   or: git branch [<options>] [-f] [--recurse-submodules] <branch-name> [<start-point>]
   or: git branch [<options>] [-l] [<pattern>...]
   or: git branch [<options>] [-r] (-d | -D) <branch-name>...
   or: git branch [<options>] (-m | -M) [<old-branch>] <new-branch>
   or: git branch [<options>] (-c | -C) [<old-branch>] <new-branch>
   or: git branch [<options>] [-r | -a] [--points-at]
   or: git branch [<options>] [-r | -a] [--format]

Generic options
    -v, --[no-]verbose    show hash and subject, give twice for upstream branch
    -q, --[no-]quiet      suppress informational messages
    -t, --[no-]track[=(direct|inherit)]
                          set branch tracking configuration
    -u, --[no-]set-upstream-to <upstream>
                          change the upstream info
    --[no-]unset-upstream unset the upstream info
    --[no-]color[=<when>] use colored output
    -r, --remotes         act on remote-tracking branches
    --contains <commit>   print only branches that contain the commit
    --no-contains <commit>
                          print only branches that don't contain the commit
    --[no-]abbrev[=<n>]   use <n> digits to display object names

Specific git-branch actions:
    -a, --all             list both remote-tracking and local branches
    -d, --[no-]delete     delete fully merged branch
    -D                    delete branch (even if not merged)
    -m, --[no-]move       move/rename a branch and its reflog
    -M                    move/rename a branch, even if target exists
    --[no-]omit-empty     do not output a newline after empty formatted refs
    -c, --[no-]copy       copy a branch and its reflog
    -C                    copy a branch, even if target exists
    -l, --[no-]list       list branch names
    --[no-]show-current   show current branch name
    --[no-]create-reflog  create the branch's reflog
    --[no-]edit-description
                          edit the description for the branch
    -f, --[no-]force      force creation, move/rename, deletion
    --merged <commit>     print only branches that are merged
    --no-merged <commit>  print only branches that are not merged
    --[no-]column[=<style>]
                          list branches in columns
    --[no-]sort <key>     field name to sort on
    --[no-]points-at <object>
                          print only branches of the object
    -i, --[no-]ignore-case
                          sorting and filtering are case insensitive
    --[no-]recurse-submodules
                          recurse through submodules
    --[no-]format <format>
                          format to use for the output

"#;

/// git's fatal error convention: `fatal: <msg>` on stderr, exit 128.
fn fatal(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

/// git's non-fatal branch-operation convention: `error: <msg>` on stderr, exit 1.
/// `git branch -d` uses this (rather than 128) for a missing or unmerged branch.
fn error_exit(msg: impl std::fmt::Display) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    Ok(ExitCode::from(1))
}

/// git's option-parsing convention: the full usage block on stderr, exit 129.
fn usage_exit() -> Result<ExitCode> {
    eprint!("{USAGE}");
    Ok(ExitCode::from(129))
}

/// The `refSyntax` advice git prints after rejecting a branch name.
fn ref_syntax_hints() {
    eprintln!("hint: See 'git help check-ref-format'");
    eprintln!("hint: Disable this message with \"git config set advice.refSyntax false\"");
}

/// Which ref namespace a listing covers. `-a`/`-r` are a single mode selector in
/// git's option table, so the last one on the command line wins.
#[derive(PartialEq, Eq, Clone, Copy)]
enum ListMode {
    Local,
    Remotes,
    All,
}

/// Parsed `git branch` command line.
struct Opts {
    mode: ListMode,
    show_current: bool,
    /// `-l`/`--list` was given explicitly, which forces list mode even with
    /// positionals (they become patterns rather than a branch to create).
    explicit_list: bool,
    verbose: u8,
    format: Option<String>,
    /// Raw `--sort=<key>` values in command-line order; an empty vec falls back
    /// to the multi-valued `branch.sort` config at listing time.
    sorts: Vec<String>,
    delete: bool,
    rename: bool,
    force: bool,
    names: Vec<String>,
}

/// One line of `git branch` output.
struct Entry<'repo> {
    /// Full ref name (`refs/heads/main`); empty for the detached-HEAD pseudo entry.
    full: BString,
    /// Name as git prints it: `main`, `origin/main` under `-r`,
    /// `remotes/origin/main` under `-a`, or `(HEAD detached at abc1234)`.
    display: String,
    /// `%(refname:short)` form — `main` / `origin/main`, without the `remotes/` prefix.
    short: String,
    /// Commit the ref points at; `None` for a symbolic ref such as `origin/HEAD`.
    id: Option<gix::Id<'repo>>,
    /// Shortened target of a symbolic ref, printed as `-> origin/main` under `-v`.
    symref: Option<String>,
    /// Whether git marks this line with `*`.
    current: bool,
    /// The detached-HEAD pseudo entry, which `--format` prints verbatim.
    detached: bool,
    /// Precomputed `--sort` values, aligned positionally with the parsed sort
    /// keys. Empty when no sort is in effect (default refname order).
    keys: Vec<SortVal>,
}

/// `git branch` — list, create, rename, and delete branches, backed by the
/// vendored gitoxide ref store.
///
/// Implemented: listing (`-a`/`--all`, `-r`/`--remotes`, `-v`/`-vv`,
/// `--format=<fmt>`, `-l`/`--list` with optional glob patterns),
/// `--sort=[-][version:]<field>` (multi-level, defaulting to the multi-valued
/// `branch.sort` config), `--show-current`, creation at HEAD, `-m`/`-M` rename,
/// and `-d`/`-D` delete.
///
/// `--sort` backs the fields whose value a branch tip (always a commit) carries:
/// `refname`, `version:refname`/`v:refname`, `committerdate`/`authordate`/
/// `creatordate`/`taggerdate`, `objectname`/`objecttype`/`objectsize`,
/// `committername`/`authorname`, the matching `*email`, and
/// `subject`/`body`/`contents`. A field name git rejects fails with git's exact
/// `unknown/malformed field name` fatal (exit 128); a field git accepts but this
/// port cannot sort by is refused rather than mis-sorted.
///
/// Not implemented, and rejected rather than ignored: `-c`/`-C` copy, `-t`/`-u`
/// upstream configuration, `--contains`/`--merged`/`--points-at` filters,
/// `--column`, `--color`, and creating a branch at an explicit
/// start-point. `--format` supports the `refname`, `refname:short`,
/// `objectname`, `objectname:short`, and `HEAD` atoms; any other atom is
/// rejected rather than rendered as empty.
///
/// The merge check for `-d` uses reachability from HEAD only (not a configured
/// upstream), which is git's behavior when no upstream is set.
pub fn branch(args: &[String]) -> Result<ExitCode> {
    let mut o = Opts {
        mode: ListMode::Local,
        show_current: false,
        explicit_list: false,
        verbose: 0,
        format: None,
        sorts: Vec::new(),
        delete: false,
        rename: false,
        force: false,
        names: Vec::new(),
    };

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--" => {
                o.names.extend(args[i + 1..].iter().cloned());
                break;
            }
            "--all" => o.mode = ListMode::All,
            "--remotes" => o.mode = ListMode::Remotes,
            "--list" => o.explicit_list = true,
            "--verbose" => o.verbose = o.verbose.saturating_add(1),
            "--show-current" => o.show_current = true,
            "--delete" => o.delete = true,
            "--move" => o.rename = true,
            "--force" => o.force = true,
            "--format" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| anyhow!("option `format' requires a value"))?;
                o.format = Some(v.clone());
            }
            _ if a.starts_with("--format=") => {
                o.format = Some(a["--format=".len()..].to_string());
            }
            "--sort" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| anyhow!("option `sort' requires a value"))?;
                o.sorts.push(v.clone());
            }
            _ if a.starts_with("--sort=") => {
                o.sorts.push(a["--sort=".len()..].to_string());
            }
            // A single-dash argument is a bundle of short flags (`-vv`, `-dr`).
            _ if a.starts_with('-') && a.len() > 1 => {
                for c in a[1..].chars() {
                    match c {
                        'a' => o.mode = ListMode::All,
                        'r' => o.mode = ListMode::Remotes,
                        'l' => o.explicit_list = true,
                        'v' => o.verbose = o.verbose.saturating_add(1),
                        'd' => o.delete = true,
                        'D' => {
                            o.delete = true;
                            o.force = true;
                        }
                        'm' => o.rename = true,
                        'M' => {
                            o.rename = true;
                            o.force = true;
                        }
                        'f' => o.force = true,
                        _ => anyhow::bail!("unsupported flag {a:?}"),
                    }
                }
            }
            _ => o.names.push(a.to_string()),
        }
        i += 1;
    }

    // git's option table marks --show-current and the list options as mutually
    // exclusive, so `--list --show-current` is a usage error before any work.
    if o.show_current && (o.explicit_list || o.delete || o.rename) {
        return usage_exit();
    }

    let repo = gix::discover(".")?;

    if o.rename {
        return rename_branch(&repo, &o);
    }
    if o.delete {
        return delete_branches(&repo, &o);
    }
    if o.show_current {
        return show_current(&repo);
    }
    if !o.names.is_empty() && !o.explicit_list {
        return create_branch(&repo, &o);
    }
    list_branches(&repo, &o)
}

/// `--show-current`: the checked-out branch's short name, or nothing at all when
/// HEAD is detached or unborn. Exits 0 either way.
fn show_current(repo: &gix::Repository) -> Result<ExitCode> {
    if let Some(name) = repo.head_name()? {
        println!("{}", name.shorten());
    }
    Ok(ExitCode::SUCCESS)
}

/// Collect the lines `git branch` would print, in git's order: the detached-HEAD
/// pseudo entry first, then refs sorted by full name (which puts `refs/heads/*`
/// ahead of `refs/remotes/*` for free).
fn collect_entries<'repo>(
    repo: &'repo gix::Repository,
    o: &Opts,
    sort_keys: &[SortKey],
) -> Result<Vec<Entry<'repo>>> {
    let head = repo.head()?;
    let current_ref: Option<BString> = head.referent_name().map(|n| n.as_bstr().to_owned());

    let mut detached: Option<Entry<'repo>> = None;
    if o.mode != ListMode::Remotes && head.is_detached() {
        if let Some(id) = head.id() {
            let display = format!("(HEAD detached at {})", id.shorten_or_id());
            detached = Some(Entry {
                full: BString::default(),
                short: display.clone(),
                display,
                id: Some(id),
                symref: None,
                current: true,
                detached: true,
                keys: Vec::new(),
            });
        }
    }

    let mut refs: Vec<Entry<'repo>> = Vec::new();

    if o.mode != ListMode::Remotes {
        for r in repo.references()?.local_branches()? {
            let r = r.map_err(|e| anyhow!("{e}"))?;
            let full = r.name().as_bstr().to_owned();
            let short = r.name().shorten().to_string();
            let (id, symref) = target_of(&r);
            refs.push(Entry {
                current: current_ref.as_ref() == Some(&full),
                full,
                display: short.clone(),
                short,
                id,
                symref,
                detached: false,
                keys: Vec::new(),
            });
        }
    }

    if o.mode != ListMode::Local {
        for r in repo.references()?.remote_branches()? {
            let r = r.map_err(|e| anyhow!("{e}"))?;
            let full = r.name().as_bstr().to_owned();
            let short = r.name().shorten().to_string();
            // Under `-a` git disambiguates remote refs with a `remotes/` prefix;
            // under `-r` the namespace is already implied.
            let display = if o.mode == ListMode::All {
                format!("remotes/{short}")
            } else {
                short.clone()
            };
            let (id, symref) = target_of(&r);
            refs.push(Entry {
                full,
                display,
                short,
                id,
                symref,
                current: false,
                detached: false,
                keys: Vec::new(),
            });
        }
    }

    // Default order is git's implicit ascending refname; an explicit sort (from
    // `--sort` or `branch.sort`) precomputes a comparable value per key per ref
    // and orders by them, with the most-significant key given last and a final
    // refname tie-break — matching git's `ref-filter` sort.
    if sort_keys.is_empty() {
        refs.sort_by(|a, b| a.full.cmp(&b.full));
    } else {
        for e in &mut refs {
            let facts = e.id.and_then(|id| commit_facts(repo, id.detach()));
            e.keys = sort_keys
                .iter()
                .map(|k| sort_value(e, facts.as_ref(), k))
                .collect();
        }
        refs.sort_by(|a, b| {
            for (idx, key) in sort_keys.iter().enumerate().rev() {
                let mut ord = a.keys[idx].cmp(&b.keys[idx]);
                if key.reverse {
                    ord = ord.reverse();
                }
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            a.full.cmp(&b.full)
        });
    }

    // `--list <pattern>...` keeps refs whose short name matches any glob. The
    // detached pseudo entry is not a ref and never matches a pattern.
    if !o.names.is_empty() {
        refs.retain(|e| {
            o.names.iter().any(|p| {
                gix::glob::wildmatch(
                    BStr::new(p.as_bytes()),
                    BStr::new(e.short.as_bytes()),
                    gix::glob::wildmatch::Mode::empty(),
                )
            })
        });
        detached = None;
    }

    let mut out = Vec::with_capacity(refs.len() + 1);
    out.extend(detached);
    out.append(&mut refs);
    Ok(out)
}

/// Split a reference's target into a commit id or a symbolic-ref short name.
fn target_of<'repo>(r: &gix::Reference<'repo>) -> (Option<gix::Id<'repo>>, Option<String>) {
    match r.target() {
        gix::refs::TargetRef::Object(_) => (Some(r.id()), None),
        gix::refs::TargetRef::Symbolic(name) => (None, Some(name.shorten().to_string())),
    }
}

/// Print the branch listing. `--format` replaces the whole line and takes
/// precedence over `-v`; `-v` pads names into a column and appends the
/// abbreviated commit and its subject.
fn list_branches(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    // git seeds the sort list from the multi-valued `branch.sort` config while
    // reading config, then appends every `--sort` from the command line. The CLI
    // keys therefore end up most significant (each key added later outranks the
    // earlier ones), yet the config keys still participate and are still
    // validated — so an invalid `branch.sort` is fatal even with a valid `--sort`.
    let mut sorts: Vec<String> = repo
        .config_snapshot()
        .plumbing()
        .values::<BString>("branch.sort")
        .unwrap_or_default()
        .into_iter()
        .map(|v| v.to_string())
        .collect();
    sorts.extend(o.sorts.iter().cloned());

    // git validates every field name while reading options/config, dying on the
    // first bad key with exit 128; a field it accepts but this port cannot back is
    // refused rather than mis-sorted.
    let sort_keys = match resolve_sort(&sorts) {
        Err(SortErr::Fatal(msg)) => return fatal(msg),
        Err(SortErr::Unsupported(spec)) => bail!("--sort={spec} is not supported by this port"),
        Ok(keys) => keys,
    };

    let entries = collect_entries(repo, o, &sort_keys)?;

    if let Some(fmt) = &o.format {
        // Render every line before printing any, so a bad atom fails the command
        // without having emitted a partial listing.
        let mut lines = Vec::with_capacity(entries.len());
        for e in &entries {
            // git prints the detached pseudo entry verbatim rather than feeding
            // it through the format, since it has no ref name to expand.
            lines.push(if e.detached {
                e.display.clone()
            } else {
                render_format(fmt, e)?
            });
        }
        for line in lines {
            println!("{line}");
        }
        return Ok(ExitCode::SUCCESS);
    }

    if o.verbose > 0 {
        // git pads every name into one column sized by the widest entry, then
        // separates it from the commit info by a single space.
        let width = entries
            .iter()
            .map(|e| e.display.chars().count())
            .max()
            .unwrap_or(0);
        for e in &entries {
            let marker = if e.current { "* " } else { "  " };
            let info = match &e.symref {
                Some(sym) => format!("-> {sym}"),
                None => verbose_info(e),
            };
            let pad = " ".repeat(width - e.display.chars().count());
            println!("{marker}{}{pad} {info}", e.display);
        }
        return Ok(ExitCode::SUCCESS);
    }

    for e in &entries {
        let marker = if e.current { "* " } else { "  " };
        println!("{marker}{}", e.display);
    }
    Ok(ExitCode::SUCCESS)
}

/// The `-v` tail: abbreviated object name and the commit subject. A tip whose
/// object cannot be read or is not a commit degrades to the abbreviation alone
/// rather than failing the whole listing.
fn verbose_info(e: &Entry<'_>) -> String {
    let Some(id) = e.id else {
        return String::new();
    };
    let abbrev = id.shorten_or_id().to_string();
    let Ok(object) = id.object() else {
        return abbrev;
    };
    let Ok(commit) = object.try_into_commit() else {
        return abbrev;
    };
    match commit.message() {
        Ok(msg) => format!("{abbrev} {}", msg.summary()),
        Err(_) => abbrev,
    }
}

/// Expand a `--format` template for one entry. Supports `%%` and the atom set
/// documented on [`branch`]; an unrecognized atom is reported as a gap rather
/// than silently expanding to nothing.
fn render_format(fmt: &str, e: &Entry<'_>) -> Result<String> {
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' && i + 1 < chars.len() && chars[i + 1] == '%' {
            out.push('%');
            i += 2;
            continue;
        }
        if chars[i] == '%' && i + 1 < chars.len() && chars[i + 1] == '(' {
            let close = chars[i + 2..]
                .iter()
                .position(|c| *c == ')')
                .ok_or_else(|| anyhow!("format: missing ')'"))?
                + i
                + 2;
            let atom: String = chars[i + 2..close].iter().collect();
            out.push_str(&atom_value(&atom, e)?);
            i = close + 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    Ok(out)
}

/// Resolve a single `%(...)` atom.
fn atom_value(atom: &str, e: &Entry<'_>) -> Result<String> {
    Ok(match atom {
        "refname" => e.full.to_string(),
        "refname:short" => e.short.clone(),
        "objectname" => e.id.map(|id| id.to_string()).unwrap_or_default(),
        "objectname:short" => e
            .id
            .map(|id| id.shorten_or_id().to_string())
            .unwrap_or_default(),
        "HEAD" => if e.current { "*" } else { " " }.to_string(),
        other => anyhow::bail!("unsupported --format atom \"%({other})\""),
    })
}

/// Create a single local branch at the current HEAD commit. A second positional
/// (start-point) is rejected rather than ignored.
fn create_branch(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    if o.names.len() > 1 {
        anyhow::bail!("creating a branch at an explicit start-point is not supported");
    }
    let name = o.names[0].as_str();
    let full = format!("refs/heads/{name}");

    if gix::validate::reference::branch_name(BStr::new(full.as_bytes())).is_err() {
        let code = fatal(format!("'{name}' is not a valid branch name"))?;
        if crate::advice::enabled("refSyntax") {
            ref_syntax_hints();
        }
        return Ok(code);
    }

    // Resolve the target commit before locking so the error path is cheap.
    let head = repo.head()?;
    if head.is_unborn() {
        return fatal("not a valid object name: 'HEAD'");
    }
    let target = head
        .id()
        .ok_or_else(|| anyhow!("HEAD does not point to a commit"))?
        .detach();

    // Serialize the ref read-modify-write through the repo coordinator.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if repo.try_find_reference(full.as_str())?.is_some() && !o.force {
        return fatal(format!("a branch named '{name}' already exists"));
    }

    repo.reference(
        full,
        target,
        if o.force {
            PreviousValue::Any
        } else {
            PreviousValue::MustNotExist
        },
        "branch: Created from HEAD",
    )?;

    Ok(ExitCode::SUCCESS)
}

/// `-m`/`-M`: rename a branch, carrying its reflog across and re-pointing HEAD
/// when the renamed branch is the checked-out one.
///
/// With one positional the current branch is renamed; with two, the first names
/// the branch to rename. git's reflog is a file keyed by ref name, so the rename
/// is a file move followed by a normal update — that preserves history where a
/// delete-and-create would drop it.
fn rename_branch(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    let (old, new) = match o.names.len() {
        0 => return usage_exit(),
        1 => {
            let Some(head) = repo.head_name()? else {
                return fatal("cannot rename the current branch while not on any");
            };
            (head.shorten().to_string(), o.names[0].clone())
        }
        2 => (o.names[0].clone(), o.names[1].clone()),
        _ => return fatal("too many arguments for a rename operation"),
    };

    let old_full = format!("refs/heads/{old}");
    let new_full = format!("refs/heads/{new}");

    if gix::validate::reference::branch_name(BStr::new(new_full.as_bytes())).is_err() {
        let code = fatal(format!("'{new}' is not a valid branch name"))?;
        if crate::advice::enabled("refSyntax") {
            ref_syntax_hints();
        }
        return Ok(code);
    }

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let mut old_ref = match repo.try_find_reference(old_full.as_str())? {
        Some(r) => r,
        None => return fatal(format!("no branch named '{old}'")),
    };
    if old_full != new_full && repo.try_find_reference(new_full.as_str())?.is_some() && !o.force {
        return fatal(format!("a branch named '{new}' already exists"));
    }
    let target = old_ref.peel_to_id_in_place()?.detach();

    let old_name: FullName = old_full
        .as_str()
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{old}': {e}"))?;
    let new_name: FullName = new_full
        .as_str()
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{new}': {e}"))?;

    let head_follows = repo.head_name()?.map(|n| n == old_name).unwrap_or(false);
    let message = format!("Branch: renamed {old_full} to {new_full}");

    // Move the reflog first so the update below appends to the carried-over
    // history rather than starting a fresh log.
    if old_full != new_full {
        let logs = repo.git_dir().join("logs");
        let from = logs.join(&old_full);
        let to = logs.join(&new_full);
        if from.exists() {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(&from, &to)?;
        }
    }

    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.clone().into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(target),
        },
        name: new_name.clone(),
        deref: false,
    })?;

    if old_full != new_full {
        repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::Any,
                log: RefLog::AndReference,
            },
            name: old_name,
            deref: false,
        })?;
    }

    if head_follows {
        repo.edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: message.into(),
                },
                expected: PreviousValue::Any,
                new: Target::Symbolic(new_name),
            },
            name: "HEAD"
                .try_into()
                .map_err(|e| anyhow!("invalid ref name 'HEAD': {e}"))?,
            deref: false,
        })?;
    }

    Ok(ExitCode::SUCCESS)
}

/// Delete one or more local branches. Without `-D`, a branch not reachable from
/// HEAD (not fully merged) is refused. The currently checked-out branch cannot
/// be deleted. Successfully deleted branches are reported as
/// `Deleted branch <name> (was <abbrev>).`; git stops at the first failure with
/// exit 1, leaving earlier deletions committed.
fn delete_branches(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    if o.names.is_empty() {
        return fatal("branch name required");
    }

    // Full ref name of the current branch (None if detached/unborn).
    let current: Option<BString> = repo.head_name()?.map(|n| n.as_bstr().to_owned());

    // Serialize all deletions through the repo coordinator, held across the loop.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    for name in &o.names {
        let full = format!("refs/heads/{name}");

        if current.as_ref().map(|c| c.as_slice()) == Some(full.as_bytes()) {
            return error_exit(format!(
                "cannot delete branch '{name}' used by worktree at '{}'",
                repo.workdir().unwrap_or_else(|| repo.git_dir()).display()
            ));
        }

        let mut reference = match repo.try_find_reference(full.as_str())? {
            Some(r) => r,
            None => return error_exit(format!("branch '{name}' not found")),
        };

        let tip_id = reference.peel_to_id_in_place()?;
        let abbrev = tip_id.shorten_or_id();
        let tip = tip_id.detach();

        if !o.force {
            let merged = match repo.head_id() {
                Ok(head_id) => match repo.merge_base(tip, head_id.detach()) {
                    Ok(base) => base.detach() == tip,
                    Err(_) => false, // no common ancestor → not merged
                },
                Err(_) => false, // unborn HEAD → nothing merged into
            };
            if !merged {
                let code = error_exit(format!("the branch '{name}' is not fully merged"))?;
                if crate::advice::enabled("forceDeleteBranch") {
                    eprintln!("hint: If you are sure you want to delete it, run 'git branch -D {name}'");
                    eprintln!(
                        "hint: Disable this message with \"git config set advice.forceDeleteBranch false\""
                    );
                }
                return Ok(code);
            }
        }

        let name_full: FullName = full
            .as_str()
            .try_into()
            .map_err(|e| anyhow!("invalid branch name '{name}': {e}"))?;
        repo.edit_reference(RefEdit {
            change: Change::Delete {
                expected: PreviousValue::Any,
                log: RefLog::AndReference,
            },
            name: name_full,
            deref: false,
        })?;

        println!("Deleted branch {name} (was {abbrev}).");
    }

    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// `--sort` / `branch.sort`
//
// git parses branch sort keys through the same `ref-filter` machinery as
// `git tag --sort` / `git for-each-ref --sort`. A branch tip is always a commit,
// so the annotated-tag layer that `git tag` needs is absent here; the field set
// below is the subset of `ref-filter`'s atoms that a commit populates.
// ---------------------------------------------------------------------------

/// git's `ref-filter.c` `valid_atom[]` field names. Membership only decides
/// git-rejects-it (`unknown field name`) vs git-accepts-it while validating a key.
const VALID_SORT_ATOMS: &[&str] = &[
    "refname",
    "objecttype",
    "objectsize",
    "objectname",
    "deltabase",
    "tree",
    "parent",
    "numparent",
    "object",
    "type",
    "tag",
    "author",
    "authorname",
    "authoremail",
    "authordate",
    "committer",
    "committername",
    "committeremail",
    "committerdate",
    "tagger",
    "taggername",
    "taggeremail",
    "taggerdate",
    "creator",
    "creatordate",
    "subject",
    "body",
    "trailers",
    "contents",
    "signature",
    "raw",
    "upstream",
    "push",
    "symref",
    "flag",
    "HEAD",
    "color",
    "worktreepath",
    "align",
    "end",
    "if",
    "then",
    "else",
    "rest",
    "ahead-behind",
    "is-base",
    "describe",
];

/// One resolved sort key.
struct SortKey {
    reverse: bool,
    kind: SortKind,
}

/// What a sort key extracts and how it compares.
enum SortKind {
    /// Compare the full refname with git's `versioncmp`.
    Version,
    /// Compare a `long` numerically (dates by seconds, size by bytes).
    Numeric(NumField),
    /// Render this atom to bytes and compare bytewise.
    Rendered(RenderField),
}

enum NumField {
    CommitterDate,
    AuthorDate,
    CreatorDate,
    /// A branch tip is a commit, so it has no tagger; the value is always 0,
    /// matching git rendering `taggerdate` as empty for a non-tag object.
    TaggerDate,
    Size,
}

/// A bytewise-compared field. `refname` reads the ref; the rest read the commit.
enum RenderField {
    Refname,
    ObjectName,
    ObjectType,
    CommitterName,
    CommitterEmail,
    AuthorName,
    AuthorEmail,
    Subject,
    Body,
    Contents,
}

/// A precomputed, comparable value for one sort key on one ref.
enum SortVal {
    Num(i64),
    Bytes(Vec<u8>),
    Version(Vec<u8>),
}

impl SortVal {
    fn cmp(&self, other: &SortVal) -> Ordering {
        match (self, other) {
            (SortVal::Num(a), SortVal::Num(b)) => a.cmp(b),
            (SortVal::Bytes(a), SortVal::Bytes(b)) => a.cmp(b),
            (SortVal::Version(a), SortVal::Version(b)) => versioncmp(a, b),
            _ => Ordering::Equal,
        }
    }
}

/// Why sort resolution failed.
enum SortErr {
    /// A field name git itself rejects: emit `fatal: {0}` and exit 128.
    Fatal(String),
    /// A field git accepts but this port cannot sort by.
    Unsupported(String),
}

/// Split a sort key into its `-` (descending), `version:`/`v:` and `*`
/// (dereference) markers and the remaining field atom.
fn parse_sort_key(key: &str) -> (bool, bool, bool, &str) {
    let mut s = key;
    let mut reverse = false;
    if let Some(rest) = s.strip_prefix('-') {
        reverse = true;
        s = rest;
    }
    let mut version = false;
    if let Some(rest) = s.strip_prefix("version:").or_else(|| s.strip_prefix("v:")) {
        version = true;
        s = rest;
    }
    let mut star = false;
    if let Some(rest) = s.strip_prefix('*') {
        star = true;
        s = rest;
    }
    (reverse, version, star, s)
}

/// git's `parse_ref_filter_atom`: an empty atom is a `malformed field name`, and
/// a field name outside `valid_atom[]` is an `unknown field name`.
fn git_sort_error(key: &str) -> Option<String> {
    let (_, _, _, atom) = parse_sort_key(key);
    if atom.is_empty() {
        return Some(format!("malformed field name: {atom}"));
    }
    let name = atom.split(':').next().unwrap_or(atom);
    if !VALID_SORT_ATOMS.contains(&name) {
        return Some(format!("unknown field name: {atom}"));
    }
    None
}

/// Validate and interpret every sort key. git dies on the first syntactically
/// invalid key in the order given, so that is checked first; only then is this
/// port's narrower support considered.
fn resolve_sort(sorts: &[String]) -> Result<Vec<SortKey>, SortErr> {
    for key in sorts {
        if let Some(msg) = git_sort_error(key) {
            return Err(SortErr::Fatal(msg));
        }
    }
    let mut keys = Vec::with_capacity(sorts.len());
    for key in sorts {
        let (reverse, version, star, atom) = parse_sort_key(key);
        let field = atom.split(':').next().unwrap_or(atom);
        // A `:suffix` (e.g. `refname:short`, `objectname:short`) changes the
        // rendered bytes git sorts on, so a field carrying one this port does not
        // interpret must be refused rather than mis-sorted. Date fields are the
        // exception: their suffix is only a display format, while git — like this
        // port — always sorts a date atom by its underlying timestamp.
        let suffixed = atom != field;
        let is_date = matches!(
            field,
            "committerdate" | "authordate" | "creatordate" | "taggerdate"
        );
        let kind = if version {
            if field == "refname" && !star && !suffixed {
                SortKind::Version
            } else {
                return Err(SortErr::Unsupported(key.clone()));
            }
        } else if star {
            // Dereference (`*field`) only differs from the plain field for an
            // annotated tag; a branch never points at one, so this port has no
            // faithful value for it.
            return Err(SortErr::Unsupported(key.clone()));
        } else if suffixed && !is_date {
            return Err(SortErr::Unsupported(key.clone()));
        } else {
            match field {
                "refname" => SortKind::Rendered(RenderField::Refname),
                "committerdate" => SortKind::Numeric(NumField::CommitterDate),
                "authordate" => SortKind::Numeric(NumField::AuthorDate),
                "creatordate" => SortKind::Numeric(NumField::CreatorDate),
                "taggerdate" => SortKind::Numeric(NumField::TaggerDate),
                "objectsize" => SortKind::Numeric(NumField::Size),
                "objectname" => SortKind::Rendered(RenderField::ObjectName),
                "objecttype" | "type" => SortKind::Rendered(RenderField::ObjectType),
                "committername" => SortKind::Rendered(RenderField::CommitterName),
                "committeremail" => SortKind::Rendered(RenderField::CommitterEmail),
                "authorname" => SortKind::Rendered(RenderField::AuthorName),
                "authoremail" => SortKind::Rendered(RenderField::AuthorEmail),
                "subject" => SortKind::Rendered(RenderField::Subject),
                "body" => SortKind::Rendered(RenderField::Body),
                "contents" => SortKind::Rendered(RenderField::Contents),
                _ => return Err(SortErr::Unsupported(key.clone())),
            }
        };
        keys.push(SortKey { reverse, kind });
    }
    Ok(keys)
}

/// The commit facts a branch sort key can read, decoded once per ref.
struct CommitFacts {
    committer_time: i64,
    author_time: i64,
    committer_name: Vec<u8>,
    committer_email: Vec<u8>,
    author_name: Vec<u8>,
    author_email: Vec<u8>,
    message: Vec<u8>,
    size: u64,
    kind: Kind,
}

/// Decode the commit a branch tip names. `None` for a symbolic ref (e.g.
/// `origin/HEAD`) or any tip that is not a readable commit — such a ref then
/// sorts with empty/zero keys and falls through to the refname tie-break.
fn commit_facts(repo: &gix::Repository, id: ObjectId) -> Option<CommitFacts> {
    let obj = repo.find_object(id).ok()?;
    let size = obj.data.len() as u64;
    let kind = obj.kind;
    if kind != Kind::Commit {
        return None;
    }
    let c = CommitRef::from_bytes(&obj.data, id.kind()).ok()?;
    let committer = c.committer().ok()?;
    let author = c.author().ok()?;
    Some(CommitFacts {
        committer_time: committer.seconds(),
        author_time: author.seconds(),
        committer_name: committer.name.to_vec(),
        committer_email: committer.email.to_vec(),
        author_name: author.name.to_vec(),
        author_email: author.email.to_vec(),
        message: c.message.to_vec(),
        size,
        kind,
    })
}

/// Compute the comparable value for one key on one ref.
fn sort_value(e: &Entry<'_>, facts: Option<&CommitFacts>, key: &SortKey) -> SortVal {
    match &key.kind {
        SortKind::Version => SortVal::Version(e.full.to_vec()),
        SortKind::Numeric(field) => {
            let n = match field {
                NumField::CommitterDate => facts.map_or(0, |f| f.committer_time),
                // A commit's creator is its committer (git's `creatordate` for a
                // non-tag object is the committer date).
                NumField::CreatorDate => facts.map_or(0, |f| f.committer_time),
                NumField::AuthorDate => facts.map_or(0, |f| f.author_time),
                NumField::TaggerDate => 0,
                NumField::Size => facts.map_or(0, |f| f.size as i64),
            };
            SortVal::Num(n)
        }
        SortKind::Rendered(field) => SortVal::Bytes(match field {
            RenderField::Refname => e.full.to_vec(),
            RenderField::ObjectName => e.id.map(|id| id.to_string().into_bytes()).unwrap_or_default(),
            RenderField::ObjectType => match facts {
                Some(f) => f.kind.as_bytes().to_vec(),
                None => Vec::new(),
            },
            RenderField::CommitterName => facts.map(|f| f.committer_name.clone()).unwrap_or_default(),
            RenderField::CommitterEmail => facts
                .map(|f| bracket_email(&f.committer_email))
                .unwrap_or_default(),
            RenderField::AuthorName => facts.map(|f| f.author_name.clone()).unwrap_or_default(),
            RenderField::AuthorEmail => facts
                .map(|f| bracket_email(&f.author_email))
                .unwrap_or_default(),
            RenderField::Subject => facts.map(|f| subject_of(&f.message)).unwrap_or_default(),
            RenderField::Body => facts.map(|f| body_of(&f.message)).unwrap_or_default(),
            RenderField::Contents => facts.map(|f| f.message.clone()).unwrap_or_default(),
        }),
    }
}

/// git's `%(committeremail)`/`%(authoremail)` wrap the address in angle brackets;
/// the sort value is the rendered atom, so match that framing.
fn bracket_email(email: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(email.len() + 2);
    v.push(b'<');
    v.extend_from_slice(email);
    v.push(b'>');
    v
}

/// git's subject: the first paragraph, with internal newlines folded to spaces.
fn subject_of(msg: &[u8]) -> Vec<u8> {
    let trimmed = {
        let end = msg.iter().rposition(|&b| b != b'\n').map_or(0, |i| i + 1);
        &msg[..end]
    };
    let sub_end = trimmed
        .windows(2)
        .position(|w| w == b"\n\n")
        .unwrap_or(trimmed.len());
    trimmed[..sub_end]
        .iter()
        .map(|&b| if b == b'\n' { b' ' } else { b })
        .collect()
}

/// git's body: everything after the blank line that ends the subject.
fn body_of(msg: &[u8]) -> Vec<u8> {
    match msg.windows(2).position(|w| w == b"\n\n") {
        Some(p) => msg[p + 2..].to_vec(),
        None => Vec::new(),
    }
}

/// git's `versioncmp` (a modified glibc `strverscmp`), byte for byte.
fn versioncmp(s1: &[u8], s2: &[u8]) -> Ordering {
    const S_N: usize = 0;
    const S_I: usize = 3;
    const S_F: usize = 6;
    const S_Z: usize = 9;
    const CMP: i8 = 2;
    const LEN: i8 = 3;
    #[rustfmt::skip]
    const NEXT_STATE: [usize; 12] = [
        S_N, S_I, S_Z,
        S_N, S_I, S_I,
        S_N, S_F, S_F,
        S_N, S_F, S_Z,
    ];
    #[rustfmt::skip]
    const RESULT_TYPE: [i8; 36] = [
        CMP, CMP, CMP, CMP, LEN, CMP, CMP, CMP, CMP,
        CMP, -1,  -1,   1,  LEN, LEN,  1,  LEN, LEN,
        CMP, CMP, CMP, CMP, CMP, CMP, CMP, CMP, CMP,
        CMP,  1,   1,  -1,  CMP, CMP, -1,  CMP, CMP,
    ];

    let get = |p: &[u8], i: usize| -> u8 { p.get(i).copied().unwrap_or(0) };
    let digit = |c: u8| -> usize { usize::from(c.is_ascii_digit()) };
    let zero = |c: u8| -> usize { usize::from(c == b'0') };

    let mut i1 = 0usize;
    let mut i2 = 0usize;
    let mut c1 = get(s1, i1);
    i1 += 1;
    let mut c2 = get(s2, i2);
    i2 += 1;
    let mut state = S_N + zero(c1) + digit(c1);
    let mut diff;
    loop {
        diff = c1 as i32 - c2 as i32;
        if diff != 0 {
            break;
        }
        if c1 == 0 {
            return Ordering::Equal;
        }
        state = NEXT_STATE[state];
        c1 = get(s1, i1);
        i1 += 1;
        c2 = get(s2, i2);
        i2 += 1;
        state += zero(c1) + digit(c1);
    }

    let rt = RESULT_TYPE[state * 3 + zero(c2) + digit(c2)];
    match rt {
        CMP => diff.cmp(&0),
        LEN => {
            loop {
                let d1 = get(s1, i1);
                i1 += 1;
                if !d1.is_ascii_digit() {
                    break;
                }
                let d2 = get(s2, i2);
                i2 += 1;
                if !d2.is_ascii_digit() {
                    return Ordering::Greater;
                }
            }
            if get(s2, i2).is_ascii_digit() {
                Ordering::Less
            } else {
                diff.cmp(&0)
            }
        }
        other => (other as i32).cmp(&0),
    }
}
