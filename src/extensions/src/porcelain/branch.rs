use anyhow::{anyhow, Result};
use std::process::ExitCode;

use gix::bstr::{BStr, BString};
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
}

/// `git branch` — list, create, rename, and delete branches, backed by the
/// vendored gitoxide ref store.
///
/// Implemented: listing (`-a`/`--all`, `-r`/`--remotes`, `-v`/`-vv`,
/// `--format=<fmt>`, `-l`/`--list` with optional glob patterns),
/// `--show-current`, creation at HEAD, `-m`/`-M` rename, and `-d`/`-D` delete.
///
/// Not implemented, and rejected rather than ignored: `-c`/`-C` copy, `-t`/`-u`
/// upstream configuration, `--contains`/`--merged`/`--points-at` filters,
/// `--sort`, `--column`, `--color`, and creating a branch at an explicit
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
fn collect_entries<'repo>(repo: &'repo gix::Repository, o: &Opts) -> Result<Vec<Entry<'repo>>> {
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
            });
        }
    }

    refs.sort_by(|a, b| a.full.cmp(&b.full));

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
    let entries = collect_entries(repo, o)?;

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
        ref_syntax_hints();
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
        ref_syntax_hints();
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
                eprintln!("hint: If you are sure you want to delete it, run 'git branch -D {name}'");
                eprintln!(
                    "hint: Disable this message with \"git config set advice.forceDeleteBranch false\""
                );
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
