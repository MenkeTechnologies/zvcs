use anyhow::{anyhow, bail, Result};
use std::cmp::Ordering;
use std::io::{IsTerminal, Write as _};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::config::{File as ConfigFile, Source};
use gix::hash::ObjectId;
use gix::objs::{CommitRef, Kind};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// git's smallest permitted abbreviation length, shared with `git blame`.
const MINIMUM_ABBREV: usize = 4;

/// The SGR reset git emits after a colored branch name (`\e[m`, not `\e[0m`).
const RESET: &str = "\x1b[m";

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

/// `-t`/`--track[=(direct|inherit)]` / `--no-track` selector, in git's option
/// order (the last one wins).
#[derive(PartialEq, Eq, Clone, Copy)]
enum Track {
    /// Neither `--track` nor `--no-track` given: auto-track per `branch.autoSetupMerge`.
    Unset,
    /// `--no-track`: never set up tracking.
    No,
    /// `-t` / `--track` / `--track=direct`: track the start-point's remote directly.
    Direct,
    /// `--track=inherit`: copy the start-point branch's own upstream configuration.
    Inherit,
}

/// `--color[=<when>]` tri-state, matching `git branch`'s default of `auto`.
#[derive(PartialEq, Eq, Clone, Copy)]
enum ColorWhen {
    Auto,
    Always,
    Never,
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
    copy: bool,
    force: bool,
    quiet: bool,
    ignore_case: bool,
    create_reflog: bool,
    edit_description: bool,
    track: Track,
    /// `-u <up>` / `--set-upstream-to=<up>`: the upstream spec to install.
    set_upstream_to: Option<String>,
    unset_upstream: bool,
    color: ColorWhen,
    /// `--abbrev=<n>` for `-v`: `None` = configured default, `Some(0)` = full hash.
    abbrev: Option<usize>,
    // Reachability filters (each entry is a raw rev spec, resolved at list time).
    contains: Vec<String>,
    no_contains: Vec<String>,
    merged: Vec<String>,
    no_merged: Vec<String>,
    points_at: Vec<String>,
    names: Vec<String>,
}

impl Opts {
    /// Whether any reachability/points-at filter is present. git forces list mode
    /// when one is, so positionals become patterns rather than a branch to create.
    fn has_filter(&self) -> bool {
        !self.contains.is_empty()
            || !self.no_contains.is_empty()
            || !self.merged.is_empty()
            || !self.no_merged.is_empty()
            || !self.points_at.is_empty()
    }
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
    /// Whether this ref lives under `refs/remotes/` (colored with the remote slot).
    remote: bool,
    /// The detached-HEAD pseudo entry, which `--format` prints verbatim.
    detached: bool,
    /// Precomputed `--sort` values, aligned positionally with the parsed sort
    /// keys. Empty when no sort is in effect (default refname order).
    keys: Vec<SortVal>,
}

/// Reachability filters resolved to concrete commit ids, as git's `ref-filter`
/// does before walking the ref list.
struct Filters {
    contains: Vec<ObjectId>,
    no_contains: Vec<ObjectId>,
    merged: Vec<ObjectId>,
    no_merged: Vec<ObjectId>,
    points_at: Vec<ObjectId>,
}

impl Filters {
    fn is_empty(&self) -> bool {
        self.contains.is_empty()
            && self.no_contains.is_empty()
            && self.merged.is_empty()
            && self.no_merged.is_empty()
            && self.points_at.is_empty()
    }
}

/// `git branch` — list, create, copy, rename, and delete branches, backed by the
/// vendored gitoxide ref store.
///
/// Implemented: listing (`-a`/`--all`, `-r`/`--remotes`, `-v`/`-vv`,
/// `--format=<fmt>`, `-l`/`--list` with optional glob patterns),
/// `--sort=[-][version:]<field>` (multi-level, defaulting to the multi-valued
/// `branch.sort` config), `--show-current`, creation at an optional
/// `<start-point>` with `-t`/`--track[=(direct|inherit)]`/`--no-track` upstream
/// setup, `-m`/`-M` rename and `-c`/`-C` copy (both carrying the reflog and the
/// `branch.<name>.*` config across), `-d`/`-D` delete, `-u`/`--set-upstream-to`
/// and `--unset-upstream`, the `--contains`/`--no-contains`/`--merged`/
/// `--no-merged`/`--points-at` reachability filters, `--abbrev[=<n>]`/
/// `--no-abbrev`, `-i`/`--ignore-case`, `-q`/`--quiet`, `--color[=<when>]`/
/// `--no-color`, and `--create-reflog`.
///
/// `--format` supports the `refname`, `refname:short`, `objectname`,
/// `objectname:short`, and `HEAD` atoms; any other atom is rejected rather than
/// rendered as empty. `--edit-description` is refused: it needs an interactive
/// editor loop that is not wired in this environment.
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
        copy: false,
        force: false,
        quiet: false,
        ignore_case: false,
        create_reflog: false,
        edit_description: false,
        track: Track::Unset,
        set_upstream_to: None,
        unset_upstream: false,
        color: ColorWhen::Auto,
        abbrev: None,
        contains: Vec::new(),
        no_contains: Vec::new(),
        merged: Vec::new(),
        no_merged: Vec::new(),
        points_at: Vec::new(),
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
            "--no-verbose" => o.verbose = 0,
            "--show-current" => o.show_current = true,
            "--delete" => o.delete = true,
            "--move" => o.rename = true,
            "--copy" => o.copy = true,
            "--force" => o.force = true,
            "--quiet" => o.quiet = true,
            "--no-quiet" => o.quiet = false,
            "--ignore-case" => o.ignore_case = true,
            "--no-ignore-case" => o.ignore_case = false,
            "--create-reflog" => o.create_reflog = true,
            "--no-create-reflog" => o.create_reflog = false,
            "--edit-description" => o.edit_description = true,
            "--unset-upstream" => o.unset_upstream = true,
            "--track" => o.track = Track::Direct,
            "--no-track" => o.track = Track::No,
            _ if a.starts_with("--track=") => {
                o.track = match &a["--track=".len()..] {
                    "direct" => Track::Direct,
                    "inherit" => Track::Inherit,
                    _ => return usage_exit(),
                };
            }
            "--color" => o.color = ColorWhen::Always,
            "--no-color" => o.color = ColorWhen::Never,
            _ if a.starts_with("--color=") => {
                o.color = match &a["--color=".len()..] {
                    "always" => ColorWhen::Always,
                    "never" | "false" => ColorWhen::Never,
                    "auto" => ColorWhen::Auto,
                    _ => return usage_exit(),
                };
            }
            "--abbrev" => o.abbrev = None,
            "--no-abbrev" => o.abbrev = Some(0),
            _ if a.starts_with("--abbrev=") => {
                let n = a["--abbrev=".len()..]
                    .parse::<usize>()
                    .map_err(|_| anyhow!("option `abbrev' expects a numerical value"))?;
                o.abbrev = Some(n);
            }
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
            "--set-upstream-to" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| anyhow!("option `set-upstream-to' requires a value"))?;
                o.set_upstream_to = Some(v.clone());
            }
            _ if a.starts_with("--set-upstream-to=") => {
                o.set_upstream_to = Some(a["--set-upstream-to=".len()..].to_string());
            }
            // The reachability filters take git's `LASTARG_DEFAULT` value: an
            // attached `=val`, else the next token, else `HEAD` when this is the
            // last argument on the command line.
            "--contains" => o.contains.push(lastarg_default(args, &mut i)),
            _ if a.starts_with("--contains=") => {
                o.contains.push(a["--contains=".len()..].to_string())
            }
            "--no-contains" => o.no_contains.push(lastarg_default(args, &mut i)),
            _ if a.starts_with("--no-contains=") => {
                o.no_contains.push(a["--no-contains=".len()..].to_string())
            }
            "--merged" => o.merged.push(lastarg_default(args, &mut i)),
            _ if a.starts_with("--merged=") => o.merged.push(a["--merged=".len()..].to_string()),
            "--no-merged" => o.no_merged.push(lastarg_default(args, &mut i)),
            _ if a.starts_with("--no-merged=") => {
                o.no_merged.push(a["--no-merged=".len()..].to_string())
            }
            "--points-at" => o.points_at.push(lastarg_default(args, &mut i)),
            _ if a.starts_with("--points-at=") => {
                o.points_at.push(a["--points-at=".len()..].to_string())
            }
            // A single-dash argument is a bundle of short flags (`-vv`, `-dr`).
            // Long options are matched explicitly above; an unrecognized `--foo`
            // falls through to the positional arm, as it did before.
            _ if a.starts_with('-') && a.len() > 1 && !a.starts_with("--") => {
                let flags = &a[1..];
                let bytes = flags.as_bytes();
                let mut ci = 0;
                while ci < bytes.len() {
                    match bytes[ci] as char {
                        'a' => o.mode = ListMode::All,
                        'r' => o.mode = ListMode::Remotes,
                        'l' => o.explicit_list = true,
                        'v' => o.verbose = o.verbose.saturating_add(1),
                        'q' => o.quiet = true,
                        'i' => o.ignore_case = true,
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
                        'c' => o.copy = true,
                        'C' => {
                            o.copy = true;
                            o.force = true;
                        }
                        't' => o.track = Track::Direct,
                        'f' => o.force = true,
                        // `-u` takes an upstream: the rest of this token, else the
                        // next argument.
                        'u' => {
                            let rest = &flags[ci + 1..];
                            let v = if rest.is_empty() {
                                i += 1;
                                args.get(i).cloned().ok_or_else(|| {
                                    anyhow!("option `set-upstream-to' requires a value")
                                })?
                            } else {
                                rest.to_string()
                            };
                            o.set_upstream_to = Some(v);
                            break;
                        }
                        _ => return usage_exit(),
                    }
                    ci += 1;
                }
            }
            _ => o.names.push(a.to_string()),
        }
        i += 1;
    }

    // git forces list mode when a reachability filter is present, so a positional
    // becomes a pattern rather than a branch to create.
    if o.has_filter() {
        o.explicit_list = true;
    }

    // git's option table marks --show-current and the list options as mutually
    // exclusive, so `--list --show-current` is a usage error before any work.
    if o.show_current && (o.explicit_list || o.delete || o.rename || o.copy) {
        return usage_exit();
    }

    let repo = gix::discover(".")?;

    if o.rename {
        return rename_branch(&repo, &o);
    }
    if o.copy {
        return copy_branch(&repo, &o);
    }
    if o.delete {
        return delete_branches(&repo, &o);
    }
    if o.edit_description {
        // --edit-description opens the configured editor on the branch
        // description; that interactive editor loop is not wired here.
        bail!("--edit-description is not supported by this port");
    }
    if let Some(up) = o.set_upstream_to.clone() {
        return set_upstream(&repo, &o, &up);
    }
    if o.unset_upstream {
        return unset_upstream(&repo, &o);
    }
    if o.show_current {
        return show_current(&repo);
    }
    if !o.names.is_empty() && !o.explicit_list {
        return create_branch(&repo, &o);
    }
    list_branches(&repo, &o)
}

/// Consume git's `LASTARG_DEFAULT` value for a bare filter flag: the next token
/// if there is one, otherwise `HEAD`. Advances `i` past a consumed token.
fn lastarg_default(args: &[String], i: &mut usize) -> String {
    if *i + 1 < args.len() {
        *i += 1;
        args[*i].clone()
    } else {
        "HEAD".to_string()
    }
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
    filters: &Filters,
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
                remote: false,
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
                remote: false,
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
                remote: true,
                detached: false,
                keys: Vec::new(),
            });
        }
    }

    // Default order is git's implicit ascending refname; an explicit sort (from
    // `--sort` or `branch.sort`) precomputes a comparable value per key per ref
    // and orders by them, with the most-significant key given last and a final
    // refname tie-break — matching git's `ref-filter` sort. `-i`/`--ignore-case`
    // folds the string comparisons the way git's icase sort does.
    let icase = o.ignore_case;
    if sort_keys.is_empty() {
        refs.sort_by(|a, b| icmp(&a.full, &b.full, icase));
    } else {
        for e in &mut refs {
            let facts = e.id.and_then(|id| commit_facts(repo, id.detach()));
            e.keys = sort_keys
                .iter()
                .map(|k| fold_sort_val(sort_value(e, facts.as_ref(), k), icase))
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
            icmp(&a.full, &b.full, icase)
        });
    }

    // `--list <pattern>...` keeps refs whose short name matches any glob. The
    // detached pseudo entry is not a ref and never matches a pattern.
    if !o.names.is_empty() {
        let mode = if icase {
            gix::glob::wildmatch::Mode::IGNORE_CASE
        } else {
            gix::glob::wildmatch::Mode::empty()
        };
        refs.retain(|e| {
            o.names.iter().any(|p| {
                gix::glob::wildmatch(BStr::new(p.as_bytes()), BStr::new(e.short.as_bytes()), mode)
            })
        });
        detached = None;
    }

    let mut out = Vec::with_capacity(refs.len() + 1);
    out.extend(detached);
    out.append(&mut refs);

    // Reachability / points-at filters, applied to every candidate line (git's
    // ref-filter drops any ref that is not a commit or fails a filter).
    if !filters.is_empty() {
        let mut kept = Vec::with_capacity(out.len());
        for e in out {
            if passes_filters(repo, filters, e.id.map(|id| id.detach()))? {
                kept.push(e);
            }
        }
        out = kept;
    }

    Ok(out)
}

/// Whether a candidate line survives the reachability/points-at filters. A line
/// with no commit id (a symbolic ref) is dropped whenever any filter is active,
/// as git does when the ref does not peel to a commit.
fn passes_filters(
    repo: &gix::Repository,
    filters: &Filters,
    id: Option<ObjectId>,
) -> Result<bool> {
    let Some(tip) = id else {
        return Ok(false);
    };
    if !filters.points_at.is_empty() && !filters.points_at.iter().any(|&o| o == tip) {
        return Ok(false);
    }
    // `--contains=<c>`: the ref must be a descendant of `<c>`.
    if !filters.contains.is_empty() {
        let mut any = false;
        for &c in &filters.contains {
            if is_ancestor(repo, c, tip)? {
                any = true;
                break;
            }
        }
        if !any {
            return Ok(false);
        }
    }
    for &c in &filters.no_contains {
        if is_ancestor(repo, c, tip)? {
            return Ok(false);
        }
    }
    // `--merged=<m>`: the ref must be reachable from `<m>`.
    if !filters.merged.is_empty() {
        let mut any = false;
        for &m in &filters.merged {
            if is_ancestor(repo, tip, m)? {
                any = true;
                break;
            }
        }
        if !any {
            return Ok(false);
        }
    }
    for &m in &filters.no_merged {
        if is_ancestor(repo, tip, m)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// git's `repo_in_merge_bases`: whether `ancestor` is reachable from `descendant`.
fn is_ancestor(repo: &gix::Repository, ancestor: ObjectId, descendant: ObjectId) -> Result<bool> {
    if ancestor == descendant {
        return Ok(true);
    }
    let bases = repo.merge_bases_many(descendant, &[ancestor])?;
    Ok(bases.into_iter().any(|b| b.detach() == ancestor))
}

/// Resolve a filter operand to the commit git compares against: parse the rev,
/// then peel to a commit. `--points-at` compares the object directly, so it does
/// not peel. The error string is git's `malformed object name` fatal body.
fn resolve_filter_commit(repo: &gix::Repository, spec: &str, peel: bool) -> Result<ObjectId> {
    let bad = || anyhow!("malformed object name '{spec}'");
    let id = repo
        .rev_parse_single(BStr::new(spec.as_bytes()))
        .map_err(|_| bad())?;
    if !peel {
        return Ok(id.detach());
    }
    let obj = id.object().map_err(|_| bad())?;
    let commit = obj.peel_to_commit().map_err(|_| bad())?;
    Ok(commit.id)
}

/// Split a reference's target into a commit id or a symbolic-ref short name.
fn target_of<'repo>(r: &gix::Reference<'repo>) -> (Option<gix::Id<'repo>>, Option<String>) {
    match r.target() {
        gix::refs::TargetRef::Object(_) => (Some(r.id()), None),
        gix::refs::TargetRef::Symbolic(name) => (None, Some(name.shorten().to_string())),
    }
}

/// Per-slot colors for the branch listing, resolved once. `on` is false when
/// coloring is disabled, in which case no SGR (and no reset) is emitted.
struct Colors {
    on: bool,
    current: String,
    local: String,
    remote: String,
}

impl Colors {
    /// The SGR to open a line's color, given its slot. Local branches default to
    /// `normal`, which renders as an empty SGR — but git still emits the reset.
    fn open(&self, e: &Entry<'_>) -> &str {
        if e.current || e.detached {
            &self.current
        } else if e.remote {
            &self.remote
        } else {
            &self.local
        }
    }
}

/// Decide whether `git branch` colors its output and, if so, resolve every slot's
/// SGR. Mirrors git: `--color` overrides, else `color.branch` falling back to
/// `color.ui` (default `auto`); `auto` colors only on a terminal.
fn resolve_colors(repo: &gix::Repository, when: ColorWhen) -> Colors {
    let on = match when {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        ColorWhen::Auto => {
            let snap = repo.config_snapshot();
            let raw = snap
                .string("color.branch")
                .or_else(|| snap.string("color.ui"))
                .map(|v| v.to_string());
            match raw.as_deref() {
                Some("always") => true,
                None | Some("auto" | "true" | "yes" | "on" | "1" | "") => {
                    std::io::stdout().is_terminal()
                }
                _ => false,
            }
        }
    };
    if !on {
        return Colors {
            on: false,
            current: String::new(),
            local: String::new(),
            remote: String::new(),
        };
    }
    let snap = repo.config_snapshot();
    let slot = |key: &str, default: &str| -> String {
        let spec = snap
            .string(key)
            .map(|v| v.to_string())
            .unwrap_or_else(|| default.to_string());
        color_sgr(&spec)
    };
    Colors {
        on: true,
        current: slot("color.branch.current", "green"),
        local: slot("color.branch.local", "normal"),
        remote: slot("color.branch.remote", "red"),
    }
}

/// Convert a git color spec (`"green"`, `"bold red"`, `"#ff00ff"`, `"reverse"`)
/// into its SGR sequence, or an empty string when the spec sets nothing visible
/// (git's `normal`). An unparsable spec yields an empty SGR rather than failing.
fn color_sgr(spec: &str) -> String {
    // git parses a spec into leading attributes then up to two colors (foreground
    // then background); the SGR emits attributes first, then the color codes.
    let mut codes: Vec<String> = Vec::new();
    let mut color_words: Vec<&str> = Vec::new();
    for word in spec.split_whitespace() {
        if let Some(code) = attr_code(word) {
            codes.push(code.to_string());
        } else {
            color_words.push(word);
        }
    }
    for (idx, word) in color_words.iter().take(2).enumerate() {
        // The foreground slot is consumed even when it renders no code (`normal`),
        // so a following color still lands in the background slot.
        if let Some(code) = color_code(word, idx == 1) {
            codes.push(code);
        }
    }
    if codes.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// git's SGR attribute codes (`color.c`), with `no`/`no-` negations.
fn attr_code(word: &str) -> Option<&'static str> {
    let (word, neg) = match word.strip_prefix("no-").or_else(|| word.strip_prefix("no")) {
        Some(rest) if !rest.is_empty() && rest != "rmal" => (rest, true),
        _ => (word, false),
    };
    Some(match (word, neg) {
        ("bold", false) => "1",
        ("dim", false) => "2",
        ("italic", false) => "3",
        ("ul", false) => "4",
        ("blink", false) => "5",
        ("reverse", false) => "7",
        ("strike", false) => "9",
        ("bold", true) | ("dim", true) => "22",
        ("italic", true) => "23",
        ("ul", true) => "24",
        ("blink", true) => "25",
        ("reverse", true) => "27",
        ("strike", true) => "29",
        ("reset", false) => "0",
        _ => return None,
    })
}

/// git's SGR color code for a name, as foreground (`bg=false`) or background.
/// `normal` produces no code (git's `-1`).
fn color_code(word: &str, bg: bool) -> Option<String> {
    let base = if bg { 40 } else { 30 };
    let bright = if bg { 100 } else { 90 };
    let (name, is_bright) = match word.strip_prefix("bright") {
        Some(rest) => (rest, true),
        None => (word, false),
    };
    let idx = match name {
        "black" => 0,
        "red" => 1,
        "green" => 2,
        "yellow" => 3,
        "blue" => 4,
        "magenta" => 5,
        "cyan" => 6,
        "white" => 7,
        "normal" if !is_bright => return None,
        "default" if !is_bright => return Some((base + 9).to_string()),
        _ => {
            if let Ok(n) = word.parse::<u8>() {
                let sel = if bg { 48 } else { 38 };
                return Some(format!("{sel};5;{n}"));
            }
            if let Some(hex) = word.strip_prefix('#') {
                if hex.len() == 6 {
                    if let (Ok(r), Ok(g), Ok(b)) = (
                        u8::from_str_radix(&hex[0..2], 16),
                        u8::from_str_radix(&hex[2..4], 16),
                        u8::from_str_radix(&hex[4..6], 16),
                    ) {
                        let sel = if bg { 48 } else { 38 };
                        return Some(format!("{sel};2;{r};{g};{b}"));
                    }
                }
            }
            return None;
        }
    };
    Some(((if is_bright { bright } else { base }) + idx).to_string())
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

    // Resolve every reachability filter before walking refs; a bad operand is
    // git's fatal 128.
    let filters = match resolve_filters(repo, o) {
        Ok(f) => f,
        Err(msg) => return fatal(msg),
    };

    let entries = collect_entries(repo, o, &sort_keys, &filters)?;

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

    let colors = resolve_colors(repo, o.color);

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
                None => verbose_info(repo, e, o.abbrev),
            };
            let pad = " ".repeat(width - e.display.chars().count());
            if colors.on {
                println!("{marker}{}{}{pad}{RESET} {info}", colors.open(e), e.display);
            } else {
                println!("{marker}{}{pad} {info}", e.display);
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    for e in &entries {
        let marker = if e.current { "* " } else { "  " };
        if colors.on {
            println!("{marker}{}{}{RESET}", colors.open(e), e.display);
        } else {
            println!("{marker}{}", e.display);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Resolve every `--contains`/`--no-contains`/`--merged`/`--no-merged`/
/// `--points-at` operand to a concrete commit id (points-at is not peeled). A bad
/// operand is reported as git's fatal string.
fn resolve_filters(repo: &gix::Repository, o: &Opts) -> Result<Filters, String> {
    let resolve = |specs: &[String], peel: bool| -> Result<Vec<ObjectId>, String> {
        specs
            .iter()
            .map(|s| resolve_filter_commit(repo, s, peel).map_err(|e| e.to_string()))
            .collect()
    };
    Ok(Filters {
        contains: resolve(&o.contains, true)?,
        no_contains: resolve(&o.no_contains, true)?,
        merged: resolve(&o.merged, true)?,
        no_merged: resolve(&o.no_merged, true)?,
        points_at: resolve(&o.points_at, false)?,
    })
}

/// The `-v` tail: abbreviated object name and the commit subject. A tip whose
/// object cannot be read or is not a commit degrades to the abbreviation alone
/// rather than failing the whole listing.
fn verbose_info(repo: &gix::Repository, e: &Entry<'_>, abbrev: Option<usize>) -> String {
    let Some(id) = e.id else {
        return String::new();
    };
    let short = abbrev_hex(repo, id, abbrev);
    let Ok(object) = id.object() else {
        return short;
    };
    let Ok(commit) = object.try_into_commit() else {
        return short;
    };
    match commit.message() {
        Ok(msg) => format!("{short} {}", msg.summary()),
        Err(_) => short,
    }
}

/// git's `find_unique_abbrev` for the `-v` object column: `--abbrev=0`/
/// `--no-abbrev` prints the full hash, an explicit `<n>` is clamped to at least
/// `MINIMUM_ABBREV` and extended to a unique prefix, and the default follows
/// `core.abbrev`.
fn abbrev_hex(repo: &gix::Repository, id: gix::Id<'_>, abbrev: Option<usize>) -> String {
    let hexsz = repo.object_hash().len_in_hex();
    let want = match abbrev {
        Some(0) => return id.detach().to_string(),
        Some(n) => n.clamp(MINIMUM_ABBREV, hexsz),
        None => return id.shorten_or_id().to_string(),
    };
    if want >= hexsz {
        return id.detach().to_string();
    }
    match gix::odb::store::prefix::disambiguate::Candidate::new(id.detach(), want)
        .ok()
        .and_then(|c| repo.objects.disambiguate_prefix(c).ok().flatten())
    {
        Some(p) => p.to_string(),
        None => id.detach().to_hex_with_len(want).to_string(),
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

/// Create a local branch. With no `<start-point>` it starts at the current HEAD
/// commit; with one, at that resolved commit. `-t`/`--track`/`branch.autoSetupMerge`
/// then records the upstream, and `-f` allows overwriting an existing branch.
fn create_branch(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    if o.names.len() > 2 {
        return usage_exit();
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

    let start = o.names.get(1).map(|s| s.as_str());

    // The reflog message names the start-point: the literal argument, or the
    // current branch's short name when starting from HEAD.
    let current_short = repo.head_name()?.map(|n| n.shorten().to_string());
    let start_name = match start {
        Some(s) => s.to_string(),
        None => current_short.clone().unwrap_or_else(|| "HEAD".to_string()),
    };

    // Resolve the target commit and, when the start-point is itself a ref, its
    // full name — used to decide tracking.
    let (target, start_ref): (ObjectId, Option<BString>) = match start {
        Some(s) => {
            let id = match repo.rev_parse_single(BStr::new(s.as_bytes())) {
                Ok(id) => id,
                Err(_) => return fatal(format!("not a valid object name: '{s}'")),
            };
            let commit = match id.object() {
                Ok(obj) => match obj.peel_to_commit() {
                    Ok(c) => c.id,
                    Err(_) => return fatal(format!("not a valid object name: '{s}'")),
                },
                Err(_) => return fatal(format!("not a valid object name: '{s}'")),
            };
            let start_ref = repo
                .find_reference(s)
                .ok()
                .map(|r| r.name().as_bstr().to_owned());
            (commit, start_ref)
        }
        None => {
            let head = repo.head()?;
            if head.is_unborn() {
                return fatal("not a valid object name: 'HEAD'");
            }
            let id = head
                .id()
                .ok_or_else(|| anyhow!("HEAD does not point to a commit"))?
                .detach();
            let start_ref = repo.head_name()?.map(|n| n.as_bstr().to_owned());
            (id, start_ref)
        }
    };

    // Decide tracking before touching the ref: git dies (without creating the
    // branch) if `--track` was explicit but the start-point is not a branch.
    let start_ref_bstr = start_ref.as_ref().map(|b| b.as_bstr());
    let upstream = tracking_upstream(repo, start_ref_bstr, o.track, name);
    if matches!(o.track, Track::Direct | Track::Inherit) && upstream.is_none() {
        return fatal(format!(
            "cannot set up tracking information; starting point '{start_name}' is not a branch"
        ));
    }

    // Serialize the ref read-modify-write through the repo coordinator.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let existed = repo.try_find_reference(full.as_str())?.is_some();
    if existed && !o.force {
        return fatal(format!("a branch named '{name}' already exists"));
    }

    let verb = if existed { "Reset to" } else { "Created from" };
    let message = format!("branch: {verb} {start_name}");

    repo.reference(
        full,
        target,
        if o.force {
            PreviousValue::Any
        } else {
            PreviousValue::MustNotExist
        },
        message,
    )?;

    if let Some(up) = upstream {
        install_tracking(repo, name, &up, o.quiet)?;
    }

    Ok(ExitCode::SUCCESS)
}

/// The upstream a branch should track: `(remote, merge_ref, short)`. Auto-set
/// when the start-point is a remote-tracking branch (git's default
/// `branch.autoSetupMerge=true`); a local branch is tracked only with an explicit
/// `--track`. `--no-track` disables it. `--track=inherit` copies the start
/// branch's own upstream. Mirrors `git switch`'s tracking logic.
fn tracking_upstream(
    repo: &gix::Repository,
    start_ref: Option<&BStr>,
    track: Track,
    new_branch: &str,
) -> Option<(String, String, String)> {
    if track == Track::No {
        return None;
    }
    let full = start_ref?;
    let s = full.to_str_lossy();
    let explicit = matches!(track, Track::Direct | Track::Inherit);

    let snap = repo.config_snapshot();
    let mode = snap
        .string("branch.autoSetupMerge")
        .map(|v| v.to_str_lossy().to_ascii_lowercase());
    let mode = mode.as_deref();
    let off = matches!(mode, Some("false" | "no" | "off" | "0"));

    // `--track=inherit` (or `branch.autoSetupMerge=inherit`) copies the start
    // branch's own upstream rather than pointing at the start branch itself.
    if track == Track::Inherit || mode == Some("inherit") {
        if let Some(b) = s.strip_prefix("refs/heads/") {
            return inherited_upstream(&snap, b);
        }
    }

    if let Some(rest) = s.strip_prefix("refs/remotes/") {
        let (remote, branch) = rest.split_once('/')?;
        let auto = if off {
            false
        } else if mode == Some("simple") {
            branch == new_branch
        } else {
            true
        };
        if explicit || auto {
            return Some((
                remote.to_string(),
                format!("refs/heads/{branch}"),
                format!("{remote}/{branch}"),
            ));
        }
        return None;
    }

    if let Some(branch) = s.strip_prefix("refs/heads/") {
        if explicit || mode == Some("always") {
            return Some((
                ".".to_string(),
                format!("refs/heads/{branch}"),
                branch.to_string(),
            ));
        }
    }
    None
}

/// The upstream inherited from a local start branch's own `branch.<b>.remote`/
/// `branch.<b>.merge`, if it has one.
fn inherited_upstream(
    snap: &gix::config::Snapshot<'_>,
    branch: &str,
) -> Option<(String, String, String)> {
    let remote = snap
        .string(&format!("branch.{branch}.remote"))?
        .to_str_lossy()
        .into_owned();
    let merge = snap
        .string(&format!("branch.{branch}.merge"))?
        .to_str_lossy()
        .into_owned();
    let short = match merge.strip_prefix("refs/heads/") {
        Some(b) if remote == "." => b.to_string(),
        Some(b) => format!("{remote}/{b}"),
        None => merge.clone(),
    };
    Some((remote, merge, short))
}

/// `-u`/`--set-upstream-to`: point a branch's upstream at `<upstream>`. Operates
/// on the given branch, or the current one when no positional is present.
fn set_upstream(repo: &gix::Repository, o: &Opts, upstream_spec: &str) -> Result<ExitCode> {
    let branch_name = match o.names.first() {
        Some(n) => n.clone(),
        None => match repo.head_name()? {
            Some(h) => h.shorten().to_string(),
            None => {
                return fatal(format!(
                    "could not set upstream of HEAD to {upstream_spec} when it does not point to any branch"
                ))
            }
        },
    };

    let full = format!("refs/heads/{branch_name}");
    if repo.try_find_reference(full.as_str())?.is_none() {
        return fatal(format!("branch '{branch_name}' does not exist"));
    }

    let up = match resolve_upstream(repo, upstream_spec)? {
        Some(u) => u,
        None => {
            let code = fatal(format!(
                "the requested upstream branch '{upstream_spec}' does not exist"
            ))?;
            if crate::advice::enabled("setUpstreamFailure") {
                eprintln!("hint:");
                eprintln!("hint: If you are planning on basing your work on an upstream");
                eprintln!("hint: branch that already exists at the remote, you may need to");
                eprintln!("hint: run \"git fetch\" to retrieve it.");
                eprintln!("hint:");
                eprintln!("hint: If you are planning to push out a new local branch that");
                eprintln!("hint: will track its remote counterpart, you may want to use");
                eprintln!("hint: \"git push -u\" to set the upstream config as you push.");
                eprintln!(
                    "hint: Disable this message with \"git config set advice.setUpstreamFailure false\""
                );
            }
            return Ok(code);
        }
    };

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    install_tracking(repo, &branch_name, &up, o.quiet)?;
    Ok(ExitCode::SUCCESS)
}

/// Resolve an upstream spec to `(remote, merge_ref, short)`. A remote-tracking
/// ref maps to its remote and the remote-side branch; a local branch maps to the
/// `.` remote. `None` when the spec does not name a ref.
fn resolve_upstream(
    repo: &gix::Repository,
    spec: &str,
) -> Result<Option<(String, String, String)>> {
    let full: BString = match repo.find_reference(spec) {
        Ok(r) => r.name().as_bstr().to_owned(),
        Err(_) => return Ok(None),
    };
    let s = full.to_str_lossy();
    if let Some(rest) = s.strip_prefix("refs/remotes/") {
        if let Some((remote, branch)) = rest.split_once('/') {
            return Ok(Some((
                remote.to_string(),
                format!("refs/heads/{branch}"),
                format!("{remote}/{branch}"),
            )));
        }
    }
    if let Some(b) = s.strip_prefix("refs/heads/") {
        return Ok(Some((".".to_string(), s.to_string(), b.to_string())));
    }
    // Any other ref (e.g. a tag): git records it against the `.` remote.
    Ok(Some((".".to_string(), s.to_string(), spec.to_string())))
}

/// `--unset-upstream`: drop `branch.<name>.remote` and `branch.<name>.merge` for
/// the given branch (or the current one). Refuses a branch with no upstream.
fn unset_upstream(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    let branch_name = match o.names.first() {
        Some(n) => n.clone(),
        None => match repo.head_name()? {
            Some(h) => h.shorten().to_string(),
            None => {
                return fatal("could not unset upstream of HEAD when it does not point to any branch")
            }
        },
    };

    let snap = repo.config_snapshot();
    let has_upstream = snap
        .string(&format!("branch.{branch_name}.remote"))
        .is_some()
        || snap
            .string(&format!("branch.{branch_name}.merge"))
            .is_some();
    if !has_upstream {
        return fatal(format!("branch '{branch_name}' has no upstream information"));
    }
    drop(snap);

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let path = repo.common_dir().join("config");
    let mut file = ConfigFile::from_path_no_includes(path.clone(), Source::Local)?;
    if let Ok(mut section) = file.section_mut("branch", Some(BStr::new(branch_name.as_bytes()))) {
        while section.remove("remote").is_some() {}
        while section.remove("merge").is_some() {}
    }
    write_config(&path, &file)?;
    Ok(ExitCode::SUCCESS)
}

/// Write `branch.<name>.remote`/`branch.<name>.merge` (and, per
/// `branch.autoSetupRebase`, `.rebase`) into the local config, then print git's
/// `set up to track` notice on stdout unless `--quiet`. Called with the repo lock
/// held.
fn install_tracking(
    repo: &gix::Repository,
    branch: &str,
    upstream: &(String, String, String),
    quiet: bool,
) -> Result<()> {
    let (remote, merge_ref, short) = upstream;
    let path = repo.common_dir().join("config");
    let mut file = ConfigFile::from_path_no_includes(path.clone(), Source::Local)?;
    let sub = BStr::new(branch.as_bytes());
    file.set_raw_value_by("branch", Some(sub), "remote", remote.as_str())?;
    file.set_raw_value_by("branch", Some(sub), "merge", merge_ref.as_str())?;

    let is_local = remote == ".";
    let want_rebase = match repo
        .config_snapshot()
        .string("branch.autoSetupRebase")
        .map(|v| v.to_str_lossy().into_owned())
        .as_deref()
    {
        Some("always") => true,
        Some("local") => is_local,
        Some("remote") => !is_local,
        _ => false,
    };
    if want_rebase {
        file.set_raw_value_by("branch", Some(sub), "rebase", "true")?;
    }

    write_config(&path, &file)?;

    if !quiet {
        println!("branch '{branch}' set up to track '{short}'.");
    }
    Ok(())
}

/// `-m`/`-M`: rename a branch, carrying its reflog and `branch.<name>.*` config
/// across and re-pointing HEAD when the renamed branch is the checked-out one.
///
/// With one positional the current branch is renamed; with two, the first names
/// the branch to rename. git's reflog is a file keyed by ref name, so the rename
/// is a file move followed by a normal update — that preserves history where a
/// delete-and-create would drop it.
fn rename_branch(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    let (old, new) = match o.names.len() {
        0 => return fatal("branch name required"),
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
                force_create_reflog: o.create_reflog,
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
        // git renames the branch's config section along with the ref.
        move_branch_config(repo, &old, &new, true)?;
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

/// `-c`/`-C`: copy a branch, duplicating its reflog and `branch.<name>.*` config
/// into the new name and leaving the source (and HEAD) untouched.
///
/// With one positional the current branch is copied; with two, the first names
/// the source. `-C` allows overwriting an existing target.
fn copy_branch(repo: &gix::Repository, o: &Opts) -> Result<ExitCode> {
    let (old, new) = match o.names.len() {
        0 => return fatal("branch name required"),
        1 => {
            let Some(head) = repo.head_name()? else {
                return fatal("cannot copy the current branch while not on any");
            };
            (head.shorten().to_string(), o.names[0].clone())
        }
        2 => (o.names[0].clone(), o.names[1].clone()),
        _ => return fatal("too many branches for a copy operation"),
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

    let new_name: FullName = new_full
        .as_str()
        .try_into()
        .map_err(|e| anyhow!("invalid branch name '{new}': {e}"))?;

    // Copy the reflog file first so the update below appends its "copied" entry to
    // the carried-over history rather than starting a fresh log.
    if old_full != new_full {
        let logs = repo.git_dir().join("logs");
        let from = logs.join(&old_full);
        let to = logs.join(&new_full);
        if from.exists() {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&from, &to)?;
        }
    }

    let message = format!("Branch: copied {old_full} to {new_full}");
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: o.create_reflog,
                message: message.into(),
            },
            expected: if o.force {
                PreviousValue::Any
            } else {
                PreviousValue::MustNotExist
            },
            new: Target::Object(target),
        },
        name: new_name,
        deref: false,
    })?;

    // git duplicates the branch's config section into the new name.
    if old_full != new_full {
        move_branch_config(repo, &old, &new, false)?;
    }

    Ok(ExitCode::SUCCESS)
}

/// Copy every `branch.<old>.*` value into `branch.<new>.*` in the local config.
/// When `remove_old`, the old subsection is deleted afterward (a rename); a copy
/// leaves it in place. Mirrors git's `git_config_copy_section` /
/// `git_config_rename_section` for the `branch.<name>` section.
fn move_branch_config(
    repo: &gix::Repository,
    old: &str,
    new: &str,
    remove_old: bool,
) -> Result<()> {
    let path = repo.common_dir().join("config");
    let mut file = ConfigFile::from_path_no_includes(path.clone(), Source::Local)?;

    // Gather the old subsection's key/value pairs in order, as owned data so the
    // immutable borrow ends before the mutation below.
    let mut pairs: Vec<(String, String)> = Vec::new();
    if let Some(iter) = file.sections_by_name("branch") {
        for section in iter {
            if section.header().subsection_name() == Some(BStr::new(old.as_bytes())) {
                for name in section.value_names() {
                    for value in section.values(&name) {
                        pairs.push((name.clone(), value.to_str_lossy().into_owned()));
                    }
                }
            }
        }
    }

    if pairs.is_empty() && !remove_old {
        return Ok(());
    }

    if !pairs.is_empty() {
        let sub = BStr::new(new.as_bytes());
        let mut section = file.section_mut_or_create_new("branch", Some(sub))?;
        for (key, value) in &pairs {
            section.push(key.as_str(), value.as_str())?;
        }
    }

    if remove_old {
        while file
            .remove_section("branch", Some(BStr::new(old.as_bytes())))
            .is_some()
        {}
    }

    write_config(&path, &file)?;
    Ok(())
}

/// Serialize `file` to `path` atomically: write a sibling temp file, then rename
/// over the target so a crash never leaves a half-written config.
fn write_config(path: &std::path::Path, file: &ConfigFile) -> Result<()> {
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

/// Delete one or more local branches. Without `-D`, a branch not reachable from
/// HEAD (not fully merged) is refused. The currently checked-out branch cannot
/// be deleted. Successfully deleted branches are reported as
/// `Deleted branch <name> (was <abbrev>).` unless `-q`; git stops at the first
/// failure with exit 1, leaving earlier deletions committed.
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

        if !o.quiet {
            println!("Deleted branch {name} (was {abbrev}).");
        }
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

/// Fold a string-valued sort key to lowercase for `-i`/`--ignore-case`; numeric
/// keys are unaffected.
fn fold_sort_val(v: SortVal, icase: bool) -> SortVal {
    if !icase {
        return v;
    }
    match v {
        SortVal::Bytes(b) => SortVal::Bytes(b.to_ascii_lowercase()),
        SortVal::Version(b) => SortVal::Version(b.to_ascii_lowercase()),
        num => num,
    }
}

/// Compare two byte strings, ASCII-folding when `icase` — the refname order used
/// for the default sort and the final tie-break.
fn icmp(a: &BString, b: &BString, icase: bool) -> Ordering {
    if icase {
        a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase())
    } else {
        a.cmp(b)
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
