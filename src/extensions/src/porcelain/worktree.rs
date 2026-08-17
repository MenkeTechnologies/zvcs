use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::{BStr, ByteSlice};
use gix::hash::ObjectId;
use gix::prelude::ObjectIdExt;
use gix::refs::FullName;

/// `git worktree` — inspect and lock the working trees attached to a repository.
///
/// Ported sub-commands (stdout, stderr and exit codes match stock git):
///   * `git worktree list`                      → aligned human-readable listing
///   * `git worktree list -v|--verbose`         → lock/prune reasons on indented lines
///   * `git worktree list --porcelain`          → machine-readable records
///   * `git worktree list --porcelain -z`       → same, NUL-terminated
///   * `git worktree list --expire <date>`      → narrow the `prunable` window
///   * `git worktree lock [--reason <s>] <wt>`  → create `worktrees/<id>/locked`
///   * `git worktree unlock <wt>`               → remove it
///   * `git worktree add <path> [<commit-ish>]` → a linked worktree, checked out
///   * `git worktree remove [-f] <wt>`          → the checkout, then its admin directory
///   * `git worktree move <wt> <new-path>`      → rename it, relinking both halves
///
/// `git worktree` itself takes no options, so any dash-prefixed token ahead of
/// the subcommand is a usage error (exit 129) — `--foo` reports an unknown
/// option, `-x` an unknown switch, and `--` reports the missing subcommand.
///
/// The listing reproduces git's `get_worktrees()`: the main worktree first (its
/// path is `realpath(common_dir)` with a trailing `/.git` stripped), then the
/// linked worktrees read out of `<common-dir>/worktrees/*/gitdir`, sorted by
/// path. Abbreviated ids honour `core.abbrev` through gitoxide's disambiguating
/// `Id::shorten()`, and the two output columns are padded to the widest value,
/// exactly as `measure_widths()` does. `prunable` is annotated when the `gitdir`
/// file points at a `.git` entry that no longer exists, the worktree is not
/// locked, and its administrative `index` is no newer than the expiry threshold
/// — the only reason reachable from `list`, since git skips a worktree whose
/// `gitdir` file is missing or empty. `list` defaults that threshold to TIME_MAX,
/// so a missing checkout is prunable unless `--expire` narrows the window.
///
/// `remove` and `move` reproduce `remove_worktree()`/`move_worktree()`: the argument
/// is resolved through the same lookup `lock` uses, the main worktree is refused, and a
/// locked one needs `-f -f` (`move`: `-f`). `remove` additionally runs
/// `check_clean_worktree()`'s question — does `status` have anything to say, tracked or
/// untracked? — and refuses without `--force`; it then deletes the checkout before the
/// administrative directory, so an interrupted removal leaves a prunable entry rather
/// than a live worktree with no bookkeeping.
///
/// `prune` and `repair` are ported: both are pure worktree-administrative
/// bookkeeping over `<common-dir>/worktrees/*`, needing no checkout. `prune`
/// reproduces `should_prune_worktree()` + `prune_dups()` and deletes stale
/// administrative directories; `repair` reproduces `repair_worktrees()` and
/// `repair_worktree_at_path()`, rewriting a worktree's `.git` gitfile and its
/// administrative `gitdir` when either drifts.
///
/// `add` reproduces `add_worktree()`: the administrative directory under
/// `worktrees/<id>` (`HEAD`, `commondir`, `gitdir`, `ORIG_HEAD`, `index`,
/// `logs/HEAD`, `refs/`), the `<path>/.git` gitfile pointing back at it, and the
/// checkout itself. `-b`/`-B`, `--detach`, `-f`, `--[no-]checkout`, `-q` and
/// `--lock [--reason]` are honoured, as is the DWIM branch named after the path's
/// last component when no `<commit-ish>` is given. The messages keep git's
/// streams: `Preparing worktree (…)` on stderr before the branch-in-use check,
/// `HEAD is now at …` on stdout from the checkout — so `--no-checkout` prints
/// only the first. `--track`, `--guess-remote`, `--orphan` and `--relative-paths`
/// are refused rather than approximated.
///
/// git creates the `-b` branch by running `git branch` in the new worktree, so
/// an option-shaped name is refused by that child rather than by `worktree`
/// itself; [`super::branch::child_branch_option_rejection`] reproduces what the
/// child says and the status it fails with, which is what keeps
/// `git worktree add -b --zzbogus <path>` from creating a ref and a checkout
/// from a typo.
///
/// NOT ported, and reported as such rather than approximated: `move` and
/// `remove`. Both need git's `check_clean_worktree()` (a `git status --porcelain`
/// of the linked tree) plus `validate_worktree()`/`update_worktree_location()`;
/// the vendored crates expose `gix_worktree_state::checkout` but not that
/// bookkeeping, so a faithful port is not possible here.
///
/// A single documented deviation: `repair <nonexistent-path>` dies (exit 128) as
/// git does, but git's `strbuf_realpath()` names the deepest resolvable path
/// component in the `fatal: Invalid path '…'` line, whereas this port names the
/// whole argument. The errno text is identical; only the quoted path differs.
///
/// Paths are rendered as lossy UTF-8; git writes the raw bytes. Column widths
/// use `char` counts where git uses `utf8_strwidth()`, so a path containing
/// double-width characters can pad differently. Both are byte-identical for the
/// ASCII paths that occur in practice.
pub fn worktree(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands us the tail *after* the verb, so the subcommand is at index
    // 0. Tolerate a leading `worktree` as well, matching the other multi-verb
    // porcelain modules, so either wiring convention works.
    let args: &[String] = match args.first() {
        Some(a) if a == "worktree" => &args[1..],
        _ => args,
    };

    let Some(sub) = args.first().map(String::as_str) else {
        return usage(Some("error: need a subcommand"), MAIN_USAGE);
    };

    // `git worktree` itself defines no options: parse_options() rejects every
    // dash-prefixed token before the subcommand. `--` ends option parsing without
    // ever producing one, so it reports the missing subcommand instead. A lone
    // `-` is not an option and falls through as a (bogus) subcommand name.
    match sub {
        // git's parse_options prints `-h` help on stdout and still exits 129.
        // `--help` is intercepted by the `git` wrapper and shows the man page,
        // which this binary has no equivalent for; the usage block is the
        // closest honest substitute.
        //
        // `--help-all` reaches the same renderer through a `strcmp()` of its
        // own in parse_options_step(), ahead of parse_long_opt(): it never
        // abbreviates and never takes an `=<value>`, so `--help-a` and
        // `--help-all=x` still fall to the unknown-option refusal below. None
        // of worktree's tables carries a `PARSE_OPT_HIDDEN` entry, so the
        // `USAGE_FULL` it renders is the block `-h` prints — here and in every
        // subcommand below.
        "-h" | "--help" | "--help-all" => {
            print!("{MAIN_USAGE}");
            return Ok(ExitCode::from(129));
        }
        "--" => return usage(Some("error: need a subcommand"), MAIN_USAGE),
        _ => {}
    }
    if let Some(long) = sub.strip_prefix("--") {
        return usage(Some(&format!("error: unknown option `{long}'")), MAIN_USAGE);
    }
    if let Some(short) = sub.strip_prefix('-').filter(|s| !s.is_empty()) {
        // git names only the offending character, not the whole cluster.
        let c = short.chars().next().unwrap_or('-');
        return usage(Some(&format!("error: unknown switch `{c}'")), MAIN_USAGE);
    }

    match sub {
        "list" => list(&args[1..]),
        "lock" => lock(&args[1..]),
        "unlock" => unlock(&args[1..]),
        "prune" => prune(&args[1..]),
        "repair" => repair(&args[1..]),
        "add" => add(&args[1..]),
        "move" => move_worktree(&args[1..]),
        "remove" => {
            remove(&args[1..])
        }
        other => usage(
            Some(&format!("error: unknown subcommand: `{other}'")),
            MAIN_USAGE,
        ),
    }
}

const MAIN_USAGE: &str = "\
usage: git worktree add [-f] [--detach] [--checkout] [--lock [--reason <string>]]
                        [--orphan] [(-b | -B) <new-branch>] <path> [<commit-ish>]
   or: git worktree list [-v | --porcelain [-z]]
   or: git worktree lock [--reason <string>] <worktree>
   or: git worktree move <worktree> <new-path>
   or: git worktree prune [-n] [-v] [--expire <expire>]
   or: git worktree remove [-f] <worktree>
   or: git worktree repair [<path>...]
   or: git worktree unlock <worktree>

";

const LIST_USAGE: &str = "\
usage: git worktree list [-v | --porcelain [-z]]

    --[no-]porcelain      machine-readable output
    -v, --[no-]verbose    show extended annotations and reasons, if available
    --[no-]expire <expiry-date>
                          add 'prunable' annotation to missing worktrees older than <time>
    -z                    terminate records with a NUL character

";

const LOCK_USAGE: &str = "\
usage: git worktree lock [--reason <string>] <worktree>

    --[no-]reason <string>
                          reason for locking

";

const UNLOCK_USAGE: &str = "\
usage: git worktree unlock <worktree>

";

const PRUNE_USAGE: &str = "\
usage: git worktree prune [-n] [-v] [--expire <expire>]

    -n, --[no-]dry-run    do not remove, show only
    -v, --[no-]verbose    report pruned working trees
    --[no-]expire <expiry-date>
                          prune missing working trees older than <time>

";

/// `add_worktree`'s table (builtin/worktree.c:804-827). `-b`/`-B` are
/// short-only `OPT_STRING`s, so neither has a `--[no-]` row.
const ADD_USAGE: &str = "\
usage: git worktree add [-f] [--detach] [--checkout] [--lock [--reason <string>]]
                        [--orphan] [(-b | -B) <new-branch>] <path> [<commit-ish>]

    -f, --[no-]force      checkout <branch> even if already checked out in other worktree
    -b <branch>           create a new branch
    -B <branch>           create or reset a branch
    --[no-]orphan         create unborn branch
    -d, --[no-]detach     detach HEAD at named commit
    --[no-]checkout       populate the new working tree
    --[no-]lock           keep the new working tree locked
    --[no-]reason <string>
                          reason for locking
    -q, --[no-]quiet      suppress progress reporting
    --[no-]track          set up tracking mode (see git-branch(1))
    --[no-]guess-remote   try to match the new branch name with a remote-tracking branch
    --[no-]relative-paths use relative paths for worktrees

";

/// `move_worktree`'s table (builtin/worktree.c:1249-1256).
const MOVE_USAGE: &str = "\
usage: git worktree move <worktree> <new-path>

    -f, --[no-]force      force move even if worktree is dirty or locked
    --[no-]relative-paths use relative paths for worktrees

";

/// `remove_worktree`'s table (builtin/worktree.c:1382-1387).
const REMOVE_USAGE: &str = "\
usage: git worktree remove [-f] <worktree>

    -f, --[no-]force      force removal even if worktree is dirty or locked

";

const REPAIR_USAGE: &str = "\
usage: git worktree repair [<path>...]

    --[no-]relative-paths use relative paths for worktrees

";

/// Print an optional `error:` line plus a usage block on stderr and exit 129,
/// which is what git's `usage_with_options()` does.
fn usage(err: Option<&str>, text: &str) -> Result<ExitCode> {
    if let Some(e) = err {
        eprintln!("{e}");
    }
    eprint!("{text}");
    Ok(ExitCode::from(129))
}

/// Print a `fatal:` line on stderr and exit 128, matching git's `die()`.
fn die(msg: &str) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

// ---------------------------------------------------------------------------
// The worktree model, mirroring `struct worktree` in git's worktree.c.
// ---------------------------------------------------------------------------

/// What `HEAD` resolves to inside one worktree.
enum HeadInfo {
    /// `HEAD` is a symref; the id is null for an unborn branch.
    Branch { oid: ObjectId, name: FullName },
    /// `HEAD` holds an object id directly.
    Detached(ObjectId),
    /// `HEAD` could not be resolved; git renders this as `(error)`.
    Unknown,
}

struct Wt {
    /// Displayed path of the checkout (the main worktree, or the linked one).
    path: PathBuf,
    /// The directory name under `worktrees/`; `None` for the main worktree.
    id: Option<String>,
    is_bare: bool,
    head: HeadInfo,
    /// `Some(reason)` when `worktrees/<id>/locked` exists; the reason may be empty.
    locked: Option<String>,
    /// `Some(reason)` when git would report the worktree as prunable.
    prunable: Option<String>,
}

impl Wt {
    fn is_linked(&self) -> bool {
        self.id.is_some()
    }

    /// The object id shown for this worktree; null when bare or unborn.
    fn oid(&self) -> ObjectId {
        match &self.head {
            HeadInfo::Branch { oid, .. } => *oid,
            HeadInfo::Detached(oid) => *oid,
            HeadInfo::Unknown => ObjectId::null(gix::hash::Kind::Sha1),
        }
    }
}

/// Trim trailing ASCII whitespace, as git's `strbuf_rtrim()` does.
fn rtrim(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[..end]
}

pub(super) fn path_to_string(p: &Path) -> String {
    gix::path::into_bstr(p).to_str_lossy().into_owned()
}

/// Read `HEAD` of `repo` the way git's `add_head_info()` does: resolve it in the
/// worktree's own ref store, keeping the symref target unpeeled.
fn head_info(repo: &gix::Repository) -> HeadInfo {
    let Ok(head) = repo.head() else {
        return HeadInfo::Unknown;
    };
    let null = ObjectId::null(repo.object_hash());
    if head.is_detached() {
        return HeadInfo::Detached(head.id().map_or(null, |id| id.detach()));
    }
    match head.referent_name() {
        Some(name) => HeadInfo::Branch {
            oid: head.id().map_or(null, |id| id.detach()),
            name: name.to_owned(),
        },
        None => HeadInfo::Unknown,
    }
}

/// Enumerate the main worktree followed by every linked worktree, sorted by
/// path — git's `get_worktrees()` plus its trailing `QSORT(list + 1, ...)`.
fn collect(repo: &gix::Repository, expire: u64) -> Result<Vec<Wt>> {
    let common = gix::path::realpath(repo.common_dir())?;

    // The main worktree's path is the common dir with a trailing `/.git` cut off,
    // which leaves a bare repository's path untouched.
    let main_path = if common.file_name().and_then(|n| n.to_str()) == Some(".git") {
        common.parent().unwrap_or(&common).to_path_buf()
    } else {
        common.clone()
    };
    let is_bare = repo.is_bare();
    let mut out = vec![Wt {
        path: main_path,
        id: None,
        is_bare,
        head: if is_bare {
            HeadInfo::Unknown
        } else {
            head_info(repo)
        },
        locked: None,
        prunable: None,
    }];

    let mut linked = Vec::new();
    let dir = match std::fs::read_dir(common.join("worktrees")) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(out);
        }
        Err(e) => return Err(e.into()),
    };
    for entry in dir {
        let entry = entry?;
        let admin = entry.path();
        let Some(id) = admin.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        // git skips a worktree whose `gitdir` file cannot be read or is empty.
        let Ok(raw) = std::fs::read(admin.join("gitdir")) else {
            continue;
        };
        let trimmed = rtrim(&raw);
        if trimmed.is_empty() {
            continue;
        }
        // The file names the worktree's `.git` entry; `should_prune_worktree()`
        // tests exactly this string for existence, before any normalisation.
        let dot_git = PathBuf::from(String::from_utf8_lossy(trimmed).into_owned());
        let missing = !dot_git.exists();

        // The checkout path drops that `/.git` suffix; a relative recording is
        // resolved against the administrative directory and then realpath'd.
        let mut path = if dot_git.file_name().and_then(|n| n.to_str()) == Some(".git") {
            dot_git.parent().unwrap_or(&dot_git).to_path_buf()
        } else {
            dot_git.clone()
        };
        if path.is_relative() {
            path = gix::path::realpath(admin.join(&path)).unwrap_or(path);
        }

        let locked_file = admin.join("locked");
        let locked = locked_file.is_file().then(|| {
            std::fs::read(&locked_file)
                .map(|b| String::from_utf8_lossy(rtrim(&b)).into_owned())
                .unwrap_or_default()
        });
        // A locked worktree is never reported prunable. Otherwise git only
        // annotates a missing checkout once it has gone stale: the administrative
        // `index` must be no newer than the expiry threshold. An unreadable
        // `index` counts as stale, matching the `stat()`-failure branch.
        let stale = || {
            let mtime = std::fs::metadata(admin.join("index"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            mtime.is_none_or(|m| m <= expire)
        };
        let prunable = (locked.is_none() && missing && stale())
            .then(|| "gitdir file points to non-existent location".to_owned());

        let head = match repo.worktree_proxy_by_id(BStr::new(id.as_str())) {
            Some(proxy) => match proxy.into_repo_with_possibly_inaccessible_worktree() {
                Ok(wt_repo) => head_info(&wt_repo),
                Err(_) => HeadInfo::Unknown,
            },
            None => HeadInfo::Unknown,
        };

        linked.push(Wt {
            path,
            id: Some(id),
            is_bare: false,
            head,
            locked,
            prunable,
        });
    }

    linked.sort_by(|a, b| a.path.cmp(&b.path));
    out.extend(linked);
    Ok(out)
}

// ---------------------------------------------------------------------------
// `git worktree list`
// ---------------------------------------------------------------------------

fn list(args: &[String]) -> Result<ExitCode> {
    let mut porcelain = false;
    let mut verbose = false;
    let mut nul = false;
    // `list` seeds the expiry at TIME_MAX, so every worktree whose `.git` entry
    // is gone counts as prunable unless `--expire` narrows the window.
    let mut expire = u64::MAX;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            // `--help-all` renders `USAGE_FULL`, the same block: no hidden entry.
            s if s == "--help" || super::asks_for_help(s, "vz") => {
                print!("{LIST_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--porcelain" => porcelain = true,
            "--no-porcelain" => porcelain = false,
            "-v" | "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            "-z" => nul = true,
            // A bare `--no-expire` resets the threshold to 0, which suppresses
            // the annotation entirely — the same value `--expire=never` yields.
            "--no-expire" => expire = 0,
            "--expire" => {
                let Some(v) = args.get(i + 1) else {
                    return usage(Some("error: option `expire' requires a value"), LIST_USAGE);
                };
                let Some(parsed) = parse_expiry(v) else {
                    return die(&format!("malformed expiration date '{v}'"));
                };
                expire = parsed;
                i += 1;
            }
            _ if a.starts_with("--expire=") => {
                let v = &a["--expire=".len()..];
                let Some(parsed) = parse_expiry(v) else {
                    return die(&format!("malformed expiration date '{v}'"));
                };
                expire = parsed;
            }
            // `--` ends option parsing; `list` takes no positionals, so anything
            // after it is a usage error and a trailing `--` is simply ignored.
            "--" => {
                if i + 1 < args.len() {
                    return usage(None, LIST_USAGE);
                }
                break;
            }
            _ if a.starts_with("--") => {
                return usage(
                    Some(&format!("error: unknown option `{}'", &a[2..])),
                    LIST_USAGE,
                );
            }
            _ if a.starts_with('-') && a.len() > 1 => {
                let c = a[1..].chars().next().unwrap_or('-');
                return usage(Some(&format!("error: unknown switch `{c}'")), LIST_USAGE);
            }
            // `list` takes no positionals; git prints the bare usage block.
            _ => return usage(None, LIST_USAGE),
        }
        i += 1;
    }

    // git checks these in this order, before touching the repository.
    if !porcelain && nul {
        return die("the option '-z' requires '--porcelain'");
    }
    if verbose && porcelain {
        return die("options '--verbose' and '--porcelain' cannot be used together");
    }

    let repo = gix::discover(".")?;
    let worktrees = collect(&repo, expire)?;

    let out = if porcelain {
        render_porcelain(&worktrees, nul)
    } else {
        render_plain(&repo, &worktrees, verbose)
    };
    print!("{out}");
    Ok(ExitCode::SUCCESS)
}

fn render_porcelain(worktrees: &[Wt], nul: bool) -> String {
    let t = if nul { '\0' } else { '\n' };
    let mut out = String::new();
    for wt in worktrees {
        out.push_str(&format!("worktree {}{t}", path_to_string(&wt.path)));
        if wt.is_bare {
            out.push_str(&format!("bare{t}"));
        } else {
            out.push_str(&format!("HEAD {}{t}", wt.oid().to_hex()));
            match &wt.head {
                HeadInfo::Detached(_) => out.push_str(&format!("detached{t}")),
                HeadInfo::Branch { name, .. } => {
                    out.push_str(&format!("branch {}{t}", name.as_bstr().to_str_lossy()));
                }
                HeadInfo::Unknown => {}
            }
        }
        if wt.is_linked() {
            if let Some(reason) = &wt.locked {
                if reason.is_empty() {
                    out.push_str(&format!("locked{t}"));
                } else if nul {
                    // Under -z git emits the reason verbatim.
                    out.push_str(&format!("locked {reason}{t}"));
                } else {
                    out.push_str(&format!("locked {}{t}", quote_c_style(reason)));
                }
            }
            if let Some(reason) = &wt.prunable {
                out.push_str(&format!("prunable {reason}{t}"));
            }
        }
        out.push(t);
    }
    out
}

fn render_plain(repo: &gix::Repository, worktrees: &[Wt], verbose: bool) -> String {
    // `measure_widths()`: the path column is the widest path, the id column the
    // longest abbreviation across every worktree.
    let paths: Vec<String> = worktrees.iter().map(|w| path_to_string(&w.path)).collect();
    let shas: Vec<String> = worktrees
        .iter()
        .map(|w| abbrev_hex(repo, w.oid()))
        .collect();
    let path_max = paths.iter().map(|p| p.chars().count()).max().unwrap_or(0);
    let sha_max = shas.iter().map(String::len).max().unwrap_or(0);

    let mut out = String::new();
    for ((wt, path), sha) in worktrees.iter().zip(&paths).zip(&shas) {
        out.push_str(path);
        out.push_str(&" ".repeat(path_max.saturating_sub(path.chars().count()) + 1));

        if wt.is_bare {
            out.push_str("(bare)");
        } else {
            out.push_str(sha);
            out.push_str(&" ".repeat(sha_max.saturating_sub(sha.len()) + 1));
            match &wt.head {
                HeadInfo::Detached(_) => out.push_str("(detached HEAD)"),
                HeadInfo::Branch { name, .. } => {
                    out.push_str(&format!("[{}]", name.as_ref().shorten().to_str_lossy()));
                }
                HeadInfo::Unknown => out.push_str("(error)"),
            }
        }

        if wt.is_linked() {
            if verbose {
                match wt.locked.as_deref() {
                    Some(r) if !r.is_empty() => out.push_str(&format!("\n\tlocked: {r}")),
                    Some(_) => out.push_str(" locked"),
                    None => {}
                }
                if let Some(r) = &wt.prunable {
                    out.push_str(&format!("\n\tprunable: {r}"));
                }
            } else {
                if wt.locked.is_some() {
                    out.push_str(" locked");
                }
                if wt.prunable.is_some() {
                    out.push_str(" prunable");
                }
            }
        }
        out.push('\n');
    }
    out
}

/// git's `parse_expiry_date()`: two keyword pairs bracket the range before
/// approxidate sees the string. `never`/`false` expire nothing (threshold 0),
/// `all`/`now` expire everything (threshold TIME_MAX) — the latter reads
/// backwards but is deliberate, since the caller wants everything already in the
/// past. `None` means the date was malformed.
fn parse_expiry(text: &str) -> Option<u64> {
    // The shared port's "expire everything" is `i64::MAX`; this caller compares against `u64`
    // mtimes, so saturate to `u64::MAX` rather than truncating the sentinel.
    match crate::date::parse_expiry_date(text)? {
        i64::MAX => Some(u64::MAX),
        seconds => Some(seconds.max(0) as u64),
    }
}

/// git's `find_unique_abbrev()`: the shortest unambiguous prefix at least
/// `core.abbrev` long. A null id has no object to disambiguate against, so git
/// simply emits that many zeroes.
fn abbrev_hex(repo: &gix::Repository, oid: ObjectId) -> String {
    if oid.is_null() {
        "0".repeat(hex_len(repo))
    } else {
        oid.attach(repo).shorten_or_id().to_string()
    }
}

/// The configured `core.abbrev`, falling back to git's automatic length which
/// scales with the number of packed objects.
fn hex_len(repo: &gix::Repository) -> usize {
    let hexsz = repo.object_hash().len_in_hex();
    let auto = || {
        let count = repo.objects.packed_object_count().unwrap_or(0);
        let bits = 64 - count.leading_zeros();
        (bits.div_ceil(2).max(7) as usize).min(hexsz)
    };
    match repo.config_snapshot().string("core.abbrev") {
        None => auto(),
        Some(value) => match &*value.to_str_lossy() {
            "auto" => auto(),
            // `core.abbrev=no|off|false` disables abbreviation entirely.
            "no" | "off" | "false" => hexsz,
            other => other
                .parse::<usize>()
                .map_or_else(|_| auto(), |n| n.clamp(4, hexsz)),
        },
    }
}

/// `quote_c_style()`: the name verbatim unless some byte needs escaping, in which
/// case the whole name double-quoted with C escapes. The table and the
/// `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn quote_c_style(text: &str) -> String {
    crate::quote::quoted_name_string(text.as_bytes())
}

// ---------------------------------------------------------------------------
// `git worktree lock` / `git worktree unlock`
// ---------------------------------------------------------------------------

fn lock(args: &[String]) -> Result<ExitCode> {
    let mut reason: Option<String> = None;
    let mut target: Option<&str> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            // `--help-all` renders `USAGE_FULL`, the same block: no hidden entry.
            s if s == "--help" || super::asks_for_help(s, "") => {
                print!("{LOCK_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--reason" => {
                let Some(v) = args.get(i + 1) else {
                    return usage(None, LOCK_USAGE);
                };
                reason = Some(v.clone());
                i += 1;
            }
            "--no-reason" => reason = None,
            _ if a.starts_with("--reason=") => reason = Some(a["--reason=".len()..].to_owned()),
            _ if a.starts_with('-') && a != "-" => return Ok(super::unknown_option(a, LOCK_USAGE)),
            _ if target.is_none() => target = Some(a),
            _ => return usage(None, LOCK_USAGE),
        }
        i += 1;
    }

    let Some(arg) = target else {
        return usage(None, LOCK_USAGE);
    };

    let repo = gix::discover(".")?;
    let worktrees = collect(&repo, u64::MAX)?;
    let Some(wt) = find_worktree(&worktrees, arg) else {
        return die(&format!("'{arg}' is not a working tree"));
    };
    let Some(id) = &wt.id else {
        return die("The main working tree cannot be locked or unlocked");
    };
    if let Some(old) = &wt.locked {
        return if old.is_empty() {
            die(&format!("'{arg}' is already locked"))
        } else {
            die(&format!("'{arg}' is already locked, reason: {old}"))
        };
    }

    // git's `write_file()` completes a non-empty payload with a newline and
    // writes nothing at all for an empty reason.
    let body = match reason.as_deref() {
        Some(r) if !r.is_empty() => format!("{r}\n"),
        _ => String::new(),
    };
    std::fs::write(
        repo.common_dir().join("worktrees").join(id).join("locked"),
        body,
    )?;
    Ok(ExitCode::SUCCESS)
}

fn unlock(args: &[String]) -> Result<ExitCode> {
    let mut target: Option<&str> = None;
    for a in args {
        match a.as_str() {
            // `--help-all` renders `USAGE_FULL`, the same block: no hidden entry.
            s if s == "--help" || super::asks_for_help(s, "") => {
                print!("{UNLOCK_USAGE}");
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with('-') && s != "-" => return Ok(super::unknown_option(s, UNLOCK_USAGE)),
            s if target.is_none() => target = Some(s),
            _ => return usage(None, UNLOCK_USAGE),
        }
    }
    let Some(arg) = target else {
        return usage(None, UNLOCK_USAGE);
    };

    let repo = gix::discover(".")?;
    let worktrees = collect(&repo, u64::MAX)?;
    let Some(wt) = find_worktree(&worktrees, arg) else {
        return die(&format!("'{arg}' is not a working tree"));
    };
    let Some(id) = &wt.id else {
        return die("The main working tree cannot be locked or unlocked");
    };
    if wt.locked.is_none() {
        return die(&format!("'{arg}' is not locked"));
    }
    std::fs::remove_file(repo.common_dir().join("worktrees").join(id).join("locked"))?;
    Ok(ExitCode::SUCCESS)
}

/// git's `find_worktree()`: try a unique path-suffix match first, then compare
/// the realpath of the argument against the realpath of each worktree.
fn find_worktree<'a>(worktrees: &'a [Wt], arg: &str) -> Option<&'a Wt> {
    if let Some(found) = find_by_suffix(worktrees, arg) {
        return Some(found);
    }
    let want = gix::path::realpath(arg).ok()?;
    worktrees
        .iter()
        .find(|wt| gix::path::realpath(&wt.path).is_ok_and(|p| p == want))
}

/// A suffix match only counts when it starts on a directory boundary, and only
/// when exactly one worktree matches.
fn find_by_suffix<'a>(worktrees: &'a [Wt], suffix: &str) -> Option<&'a Wt> {
    if suffix.is_empty() {
        return None;
    }
    let mut found = None;
    let mut hits = 0usize;
    for wt in worktrees {
        let path = path_to_string(&wt.path);
        let Some(start) = path.len().checked_sub(suffix.len()) else {
            continue;
        };
        if !path.is_char_boundary(start) {
            continue;
        }
        let boundary = start == 0 || path.as_bytes()[start - 1] == b'/';
        if boundary && path[start..] == *suffix {
            found = Some(wt);
            hits += 1;
            if hits > 1 {
                return None;
            }
        }
    }
    (hits == 1).then_some(found).flatten()
}

// ---------------------------------------------------------------------------
// Shared helpers for the administrative-bookkeeping subcommands.
// ---------------------------------------------------------------------------

/// The raw bytes of a path, as git stores them in `gitdir`/`.git` files. git
/// writes and compares the on-disk bytes; `path_to_string`'s lossy rendering is
/// only used for the human-facing `repair:`/`Removing` lines.
fn path_bytes(p: &Path) -> Vec<u8> {
    Vec::from(gix::path::into_bstr(p).into_owned())
}

/// Strip the `\n`/`\r` run off the end of a `(os error N)` suffix so an I/O
/// error reads like git's `strerror()` text rather than Rust's `Display`.
fn errno_str(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(idx) => s[..idx].to_owned(),
        None => s,
    }
}

/// git's `is_git_directory()`: gitoxide's `is_git` performs the same
/// HEAD/commondir/objects/refs probe, so a linked worktree's administrative
/// directory validates while a moved-away backlink does not.
fn is_git_dir(p: &Path) -> bool {
    gix::discover::is_git(p).is_ok()
}

/// Error codes from git's `read_gitfile_gently()` (setup.c), only the ones the
/// worktree callers branch on.
#[derive(PartialEq, Eq)]
enum GitfileErr {
    StatFailed,
    NotAFile,
    OpenFailed,
    ReadFailed,
    InvalidFormat,
    NoPath,
    NotARepo,
}

/// Port of `read_gitfile_gently()`: read a `gitdir: <path>` gitfile and return
/// the realpath of the git directory it names, or the matching error code. A
/// relative `<path>` is resolved against the gitfile's own directory, then the
/// result is validated with `is_git_directory()` before it is realpath'd.
fn read_gitfile(path: &Path) -> Result<PathBuf, GitfileErr> {
    let st = std::fs::metadata(path).map_err(|_| GitfileErr::StatFailed)?;
    if !st.is_file() {
        return Err(GitfileErr::NotAFile);
    }
    // git rejects a > 1MB gitfile as READ_GITFILE_ERR_TOO_LARGE. Neither
    // worktree caller distinguishes it from the other broken-file codes (both
    // map every non-`NotAFile`/`NotARepo` error to "broken"), so it is folded
    // into `ReadFailed` here rather than given its own variant.
    if st.len() > (1 << 20) {
        return Err(GitfileErr::ReadFailed);
    }
    let buf = std::fs::read(path).map_err(|_| GitfileErr::OpenFailed)?;
    if buf.len() as u64 != st.len() {
        return Err(GitfileErr::ReadFailed);
    }
    if !buf.starts_with(b"gitdir: ") {
        return Err(GitfileErr::InvalidFormat);
    }
    let mut len = buf.len();
    while len > 0 && (buf[len - 1] == b'\n' || buf[len - 1] == b'\r') {
        len -= 1;
    }
    if len < 9 {
        return Err(GitfileErr::NoPath);
    }
    let named = gix::path::from_byte_slice(&buf[8..len]);
    let dir = if named.is_absolute() {
        named.to_path_buf()
    } else {
        match path.parent() {
            Some(parent) => parent.join(named),
            None => named.to_path_buf(),
        }
    };
    if !is_git_dir(&dir) {
        return Err(GitfileErr::NotARepo);
    }
    gix::path::realpath(&dir).map_err(|_| GitfileErr::NotARepo)
}

// ---------------------------------------------------------------------------
// `git worktree prune`
// ---------------------------------------------------------------------------

fn prune(args: &[String]) -> Result<ExitCode> {
    let mut show_only = false;
    let mut verbose = false;
    // git seeds `expire = TIME_MAX`, so every stale worktree prunes unless
    // `--expire` narrows the window.
    let mut expire = u64::MAX;
    let mut positional = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            // `--help-all` renders `USAGE_FULL`, the same block: no hidden entry.
            s if s == "--help" || super::asks_for_help(s, "nv") => {
                print!("{PRUNE_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-n" | "--dry-run" => show_only = true,
            "--no-dry-run" => show_only = false,
            "-v" | "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            // `--no-expire` resets the threshold to 0 (expire nothing).
            "--no-expire" => expire = 0,
            "--expire" => {
                let Some(v) = args.get(i + 1) else {
                    return usage(Some("error: option `expire' requires a value"), PRUNE_USAGE);
                };
                let Some(parsed) = parse_expiry(v) else {
                    return die(&format!("malformed expiration date '{v}'"));
                };
                expire = parsed;
                i += 1;
            }
            _ if a.starts_with("--expire=") => {
                let v = &a["--expire=".len()..];
                let Some(parsed) = parse_expiry(v) else {
                    return die(&format!("malformed expiration date '{v}'"));
                };
                expire = parsed;
            }
            // `--` ends option parsing; anything after it is a leftover
            // positional, and `prune` accepts none.
            "--" => {
                if i + 1 < args.len() {
                    positional = true;
                }
                break;
            }
            _ if a.starts_with("--") => {
                return usage(
                    Some(&format!("error: unknown option `{}'", &a[2..])),
                    PRUNE_USAGE,
                );
            }
            _ if a.starts_with('-') && a.len() > 1 => {
                let c = a[1..].chars().next().unwrap_or('-');
                return usage(Some(&format!("error: unknown switch `{c}'")), PRUNE_USAGE);
            }
            _ => positional = true,
        }
        i += 1;
    }

    // git: `if (ac) usage_with_options(...)` — any leftover positional prints
    // the bare usage block (no `error:` line) and exits 129.
    if positional {
        return usage(None, PRUNE_USAGE);
    }

    let repo = gix::discover(".")?;
    prune_worktrees(&repo, show_only, verbose, expire);
    Ok(ExitCode::SUCCESS)
}

/// The verdict for one administrative directory, mirroring the `should_prune`
/// / `*wtpath` outputs of git's `should_prune_worktree()`.
enum PruneCheck {
    /// Prune the entry; the string is the reason shown under `-n`/`-v`.
    Prune(String),
    /// Keep it; `Some(bytes)` is the recorded `.git` path used for dup
    /// detection, `None` when the entry is locked (git leaves `*wtpath` NULL).
    Keep(Option<Vec<u8>>),
}

/// Port of `prune_worktrees()`: prune each stale administrative directory, then
/// `prune_dups()` over the survivors plus the main worktree, then drop an empty
/// `worktrees/` directory.
fn prune_worktrees(repo: &gix::Repository, show_only: bool, verbose: bool, expire: u64) {
    let common = repo.common_dir();
    let wt_dir = common.join("worktrees");

    let mut kept: Vec<(Vec<u8>, Option<String>)> = Vec::new();
    if let Ok(dir) = std::fs::read_dir(&wt_dir) {
        for entry in dir.flatten() {
            // git keys on the raw dirent name; a non-UTF-8 administrative id
            // never occurs (git mints ASCII basenames), so skipping it here is
            // safe.
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            match should_prune(&entry.path(), expire) {
                PruneCheck::Prune(reason) => {
                    prune_worktree(&wt_dir, &id, &reason, show_only, verbose)
                }
                PruneCheck::Keep(Some(path)) => kept.push((path, Some(id))),
                PruneCheck::Keep(None) => {}
            }
        }
    }

    // The main worktree can never be pruned; it participates in dup detection
    // only. git: absolute path of the common dir with a trailing `/.` removed.
    kept.push((main_worktree_path(common), None));
    prune_dups(&wt_dir, &kept, show_only, verbose);

    if !show_only {
        let _ = std::fs::remove_dir(&wt_dir); // rmdir; ignore failure, as git does
    }
}

/// Port of `should_prune_worktree()` (worktree.c). Reason strings are verbatim.
fn should_prune(admin: &Path, expire: u64) -> PruneCheck {
    if !admin.is_dir() {
        return PruneCheck::Prune("not a valid directory".to_owned());
    }
    if admin.join("locked").exists() {
        return PruneCheck::Keep(None);
    }
    let gitdir = admin.join("gitdir");
    let st = match std::fs::metadata(&gitdir) {
        Ok(s) => s,
        Err(_) => return PruneCheck::Prune("gitdir file does not exist".to_owned()),
    };
    let content = match std::fs::read(&gitdir) {
        Ok(c) => c,
        Err(e) => {
            return PruneCheck::Prune(format!("unable to read gitdir file ({})", errno_str(&e)))
        }
    };
    if content.len() as u64 != st.len() {
        return PruneCheck::Prune(format!(
            "short read (expected {} bytes, read {})",
            st.len(),
            content.len()
        ));
    }
    let mut len = content.len();
    while len > 0 && (content[len - 1] == b'\n' || content[len - 1] == b'\r') {
        len -= 1;
    }
    if len == 0 {
        return PruneCheck::Prune("invalid gitdir file".to_owned());
    }
    let recorded = &content[..len];
    let target = gix::path::from_byte_slice(recorded);
    if target.exists() {
        return PruneCheck::Keep(Some(recorded.to_vec()));
    }
    // A missing checkout only prunes once its administrative `index` has gone
    // stale: `stat()` failure, or mtime no newer than the expiry threshold.
    let stale = match std::fs::metadata(admin.join("index")).and_then(|m| m.modified()) {
        Err(_) => true,
        Ok(t) => t
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .map_or(true, |m| m <= expire),
    };
    if stale {
        PruneCheck::Prune("gitdir file points to non-existent location".to_owned())
    } else {
        PruneCheck::Keep(Some(recorded.to_vec()))
    }
}

/// Port of `prune_worktree()`: announce under `-n`/`-v`, delete unless dry-run.
fn prune_worktree(wt_dir: &Path, id: &str, reason: &str, show_only: bool, verbose: bool) {
    if show_only || verbose {
        eprintln!("Removing worktrees/{}: {}", id, reason);
    }
    if !show_only {
        delete_git_dir(wt_dir, id);
    }
}

/// Port of `delete_git_dir()`: recursively remove the administrative directory,
/// falling back to `unlink` for a stray non-directory entry (git's `ENOTDIR`
/// branch).
fn delete_git_dir(wt_dir: &Path, id: &str) {
    let path = wt_dir.join(id);
    let res = if path.is_dir() {
        std::fs::remove_dir_all(&path)
    } else {
        std::fs::remove_file(&path)
    };
    if let Err(e) = res {
        eprintln!(
            "error: failed to delete '{}': {}",
            path_to_string(&path),
            errno_str(&e)
        );
    }
}

/// git's `strbuf_add_absolute_path(get_git_common_dir())` with a trailing `/.`
/// stripped — the path form the recorded `gitdir` content is compared against.
fn main_worktree_path(common: &Path) -> Vec<u8> {
    let abs = if common.is_absolute() {
        common.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(common))
            .unwrap_or_else(|_| common.to_path_buf())
    };
    let mut bytes = path_bytes(&abs);
    if bytes.ends_with(b"/.") {
        bytes.truncate(bytes.len() - 2);
    }
    bytes
}

/// Port of `prune_dups()`: sort by (path, main-first, id) and prune every entry
/// whose recorded path duplicates its predecessor's.
fn prune_dups(
    wt_dir: &Path,
    kept: &[(Vec<u8>, Option<String>)],
    show_only: bool,
    verbose: bool,
) {
    let mut sorted: Vec<&(Vec<u8>, Option<String>)> = kept.iter().collect();
    sorted.sort_by(|a, b| match a.0.cmp(&b.0) {
        std::cmp::Ordering::Equal => match (&a.1, &b.1) {
            // The main worktree (`util == NULL`) sorts above linked ones, so it
            // is never the entry chosen for pruning within a duplicate run.
            (None, _) => std::cmp::Ordering::Less,
            (_, None) => std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => x.cmp(y),
        },
        other => other,
    });
    for i in 1..sorted.len() {
        if sorted[i].0 == sorted[i - 1].0 {
            if let Some(id) = &sorted[i].1 {
                prune_worktree(wt_dir, id, "duplicate entry", show_only, verbose);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// `git worktree repair`
// ---------------------------------------------------------------------------

fn repair(args: &[String]) -> Result<ExitCode> {
    // `repair`'s only option is `OPT_BOOL(0, "relative-paths", …)`; every other
    // dash-prefixed token is an unknown option.
    let mut paths: Vec<&str> = Vec::new();
    // `use_relative_paths` is a file-scope static seeded from
    // `worktree.useRelativePaths` by `git_worktree_config()`, which the option
    // then overrides.
    let mut relative: Option<bool> = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            // `--help-all` renders `USAGE_FULL`, the same block: no hidden entry.
            s if s == "--help" || super::asks_for_help(s, "") => {
                print!("{REPAIR_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--relative-paths" => relative = Some(true),
            "--no-relative-paths" => relative = Some(false),
            "--" => {
                i += 1;
                while i < args.len() {
                    paths.push(args[i].as_str());
                    i += 1;
                }
                break;
            }
            _ if a.starts_with("--") => {
                return usage(
                    Some(&format!("error: unknown option `{}'", &a[2..])),
                    REPAIR_USAGE,
                );
            }
            _ if a.starts_with('-') && a.len() > 1 => {
                let c = a[1..].chars().next().unwrap_or('-');
                return usage(Some(&format!("error: unknown switch `{c}'")), REPAIR_USAGE);
            }
            _ => paths.push(a),
        }
        i += 1;
    }

    let repo = gix::discover(".")?;
    let common = repo.common_dir().to_path_buf();
    // `git_worktree_config()`: `worktree.useRelativePaths` is the default the
    // `--relative-paths` option overrides.
    let relative = relative.unwrap_or_else(|| {
        repo.config_snapshot()
            .boolean("worktree.useRelativePaths")
            .unwrap_or(false)
    });

    let mut rc: i32 = 0;
    // git: `p = ac > 0 ? av : {"."}`.
    let targets: Vec<&str> = if paths.is_empty() { vec!["."] } else { paths };
    for p in targets {
        if let Err(code) = repair_worktree_at_path(&common, Path::new(p), &mut rc, relative) {
            return Ok(code);
        }
    }
    repair_worktrees(&repo, &common, &mut rc, relative);

    Ok(if rc != 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Emit one repair report line, matching git's `report_repair()`:
/// `repair: <msg>: <path>` for a fix, `error: <msg>: <path>` (and exit 1) for a
/// failure.
fn report(rc: &mut i32, iserr: bool, path: &Path, msg: &str) {
    if iserr {
        eprintln!("error: {}: {}", msg, path_to_string(path));
        *rc = 1;
    } else {
        eprintln!("repair: {}: {}", msg, path_to_string(path));
    }
}

/// git's `write_file()` for the two worktree callers: `<prefix><value>\n`.
fn write_gitfile(path: &Path, prefix: &[u8], value: &Path) {
    let mut body = Vec::from(prefix);
    body.extend_from_slice(&path_bytes(value));
    body.push(b'\n');
    let _ = std::fs::write(path, body);
}

/// Port of `write_worktree_linking_files()`: rewrite the pair of files that link
/// a worktree to its administrative directory — `<wt>/.git` (`gitdir: <repo>`)
/// and `<repo>/gitdir` (`<wt>/.git`) — either side absolute or relative to the
/// other, per `worktree.useRelativePaths` / `--relative-paths`.
///
/// Relative linking needs the `relativeWorktrees` repository extension, because a
/// git that does not understand it would resolve the relative gitdir against the
/// wrong directory; git upgrades the format to 1 and sets the extension before
/// writing, and so does this.
fn write_worktree_linking_files(common: &Path, dotgit: &Path, gitdir: &Path, relative: bool) {
    // `strbuf_strip_suffix` + `strbuf_realpath` on both sides: `path` is the
    // worktree root, `repo` the administrative directory.
    let path = strip_suffix(dotgit, "/.git");
    let path = gix::path::realpath(&path).unwrap_or(path);
    let repo = strip_suffix(gitdir, "/gitdir");
    let repo = gix::path::realpath(&repo).unwrap_or(repo);

    if relative {
        enable_relative_worktrees(common);
        write_gitfile(gitdir, b"", &relative_path(&path.join(".git"), &repo));
        write_gitfile(dotgit, b"gitdir: ", &relative_path(&repo, &path));
    } else {
        write_gitfile(gitdir, b"", &path.join(".git"));
        write_gitfile(dotgit, b"gitdir: ", &repo);
    }
}

/// `upgrade_repository_format(1)` + `extensions.relativeWorktrees=true`, which
/// git performs before it first writes a relative link. Both land in the
/// *common* config, since the extension describes the repository, not a worktree.
fn enable_relative_worktrees(common: &Path) {
    use gix::config::{File as ConfigFile, Source};

    let path = common.join("config");
    let Ok(mut file) = ConfigFile::from_path_no_includes(path.clone(), Source::Local) else {
        return;
    };
    if file.boolean("extensions.relativeWorktrees").ok().flatten() == Some(true) {
        return;
    }
    if file
        .set_raw_value_by("core", None, "repositoryformatversion", "1")
        .is_err()
        || file
            .set_raw_value_by("extensions", None, "relativeWorktrees", "true")
            .is_err()
    {
        return;
    }
    let tmp = path.with_extension("zvcs-tmp");
    if std::fs::write(&tmp, file.to_bstring()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Remove a trailing `suffix` from a path's bytes (`strbuf_strip_suffix`).
fn strip_suffix(p: &Path, suffix: &str) -> PathBuf {
    let bytes = path_bytes(p);
    let trimmed = match bytes.strip_suffix(suffix.as_bytes()) {
        Some(rest) => rest,
        None => &bytes,
    };
    gix::path::from_byte_slice(trimmed).to_path_buf()
}

/// Port of `relative_path()`: `target` expressed relative to the directory
/// `base`, both already absolute. Components shared with `base` are dropped, each
/// remaining `base` component becomes a `../`, and a `base` that is not a prefix
/// of `target` still resolves because every unmatched component contributes one
/// `../`. git returns `./` when the two are the same path.
pub(super) fn relative_path(target: &Path, base: &Path) -> PathBuf {
    let t: Vec<_> = target.components().collect();
    let b: Vec<_> = base.components().collect();
    let shared = t.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in shared..b.len() {
        out.push("..");
    }
    for c in &t[shared..] {
        out.push(c);
    }
    if out.as_os_str().is_empty() {
        return PathBuf::from("./");
    }
    out
}

/// Port of `repair_worktrees()`: fix each linked worktree's `.git` gitfile
/// (skipping the main worktree, git's `worktrees + 1`).
fn repair_worktrees(repo: &gix::Repository, common: &Path, rc: &mut i32, relative: bool) {
    let Ok(worktrees) = collect(repo, u64::MAX) else {
        return;
    };
    for wt in worktrees.iter().filter(|w| w.is_linked()) {
        if let Some(id) = &wt.id {
            repair_gitfile(common, id, &wt.path, rc, relative);
        }
    }
}

/// Port of `repair_gitfile()`: rewrite `<wt>/.git` when it is missing, broken,
/// or points somewhere other than `realpath(worktrees/<id>)`.
fn repair_gitfile(common: &Path, id: &str, wt_path: &Path, rc: &mut i32, relative: bool) {
    // A missing checkout can't be repaired.
    if !wt_path.exists() {
        return;
    }
    if !wt_path.is_dir() {
        report(rc, true, wt_path, "not a directory");
        return;
    }
    let admin = common.join("worktrees").join(id);
    let repo_dir = gix::path::realpath(&admin).unwrap_or(admin);
    let dotgit = wt_path.join(".git");

    let repair: Option<&str> = match read_gitfile(&dotgit) {
        Err(GitfileErr::NotAFile) => {
            report(rc, true, wt_path, ".git is not a file");
            return;
        }
        Err(_) => Some(".git file broken"),
        Ok(backlink) => {
            if path_bytes(&backlink) != path_bytes(&repo_dir) {
                Some(".git file incorrect")
            } else if relative {
                // `use_relative_paths == is_absolute_path(dotgit_contents)`.
                // `dotgit_contents` is what `read_gitfile_gently()` returned,
                // which is the *resolved* directory and therefore always
                // absolute — so this arm fires whenever relative linking was
                // asked for, and never otherwise. That is why stock
                // `worktree repair --relative-paths` re-reports the same
                // worktree on every run, while a plain `worktree repair` leaves
                // an already-relative link alone instead of making it absolute.
                Some(".git file absolute/relative path mismatch")
            } else {
                None
            }
        }
    };
    if let Some(msg) = repair {
        report(rc, false, wt_path, msg);
        write_worktree_linking_files(common, &dotgit, &repo_dir.join("gitdir"), relative);
    }
}

/// Port of `repair_worktree_at_path()`: rewrite `worktrees/<id>/gitdir` when it
/// is unreadable or points somewhere other than `realpath(<path>/.git)`. `Err`
/// carries an exit code for the (rare) fatal case that git reaches via
/// `strbuf_add_real_path()` on a non-resolvable path argument.
fn repair_worktree_at_path(
    common: &Path,
    path: &Path,
    rc: &mut i32,
    relative: bool,
) -> Result<(), ExitCode> {
    // is_main_worktree_path(): git realpaths the argument with die-on-error and
    // compares the `/.git`-stripped result against the common dir.
    let target = match gix::path::realpath(path) {
        Ok(t) => t,
        Err(_) => {
            let reason = std::fs::metadata(path)
                .err()
                .map(|e| errno_str(&e))
                .unwrap_or_else(|| "invalid path".to_owned());
            eprintln!("fatal: Invalid path '{}': {}", path_to_string(path), reason);
            return Err(ExitCode::from(128));
        }
    };
    if is_main_worktree(common, &target) {
        return Ok(());
    }

    let dotgit = path.join(".git");
    let realdotgit = match gix::path::realpath(&dotgit) {
        Ok(r) => r,
        // strbuf_realpath(die_on_error=0): reported, not fatal.
        Err(_) => {
            report(rc, true, path, "not a valid path");
            return Ok(());
        }
    };

    let backlink = match read_gitfile(&realdotgit) {
        Err(GitfileErr::NotAFile) => {
            report(
                rc,
                true,
                &realdotgit,
                "unable to locate repository; .git is not a file",
            );
            return Ok(());
        }
        // Both trees moved: infer the backlink from the recorded id.
        Err(GitfileErr::NotARepo) => match infer_backlink(common, &realdotgit) {
            Some(b) => b,
            None => {
                report(
                    rc,
                    true,
                    &realdotgit,
                    "unable to locate repository; .git file does not reference a repository",
                );
                return Ok(());
            }
        },
        Err(_) => {
            report(
                rc,
                true,
                &realdotgit,
                "unable to locate repository; .git file broken",
            );
            return Ok(());
        }
        Ok(b) => b,
    };

    let gitdir = backlink.join("gitdir");
    let repair: Option<&str> = match std::fs::read(&gitdir) {
        Err(_) => Some("gitdir unreadable"),
        // Unlike the `.git`-file side, this compares the *raw* recorded bytes
        // (`strbuf_read_file`), so `use_relative_paths == is_absolute_path(…)`
        // really does detect a link written the other way round.
        Ok(old) if relative == gix::path::from_byte_slice(rtrim(&old)).is_absolute() => {
            Some("gitdir absolute/relative path mismatch")
        }
        Ok(old) => {
            // A relative recording is resolved against the administrative
            // directory before it is compared.
            let recorded = gix::path::from_byte_slice(rtrim(&old)).to_path_buf();
            let resolved = if recorded.is_absolute() {
                recorded
            } else {
                let joined = backlink.join(&recorded);
                gix::path::realpath(&joined).unwrap_or(joined)
            };
            if path_bytes(&resolved) != path_bytes(&realdotgit) {
                Some("gitdir incorrect")
            } else {
                None
            }
        }
    };
    if let Some(msg) = repair {
        report(rc, false, &gitdir, msg);
        write_worktree_linking_files(common, &realdotgit, &gitdir, relative);
    }
    Ok(())
}

/// Port of `is_main_worktree_path()`: compare the `/.git`-stripped realpaths of
/// the (already resolved) argument and the common dir.
fn is_main_worktree(common: &Path, target_realpath: &Path) -> bool {
    let main = gix::path::realpath(common).unwrap_or_else(|_| common.to_path_buf());
    strip_dotgit(target_realpath) == strip_dotgit(&main)
}

/// Remove a trailing `/.git` from a path's bytes, as `strbuf_strip_suffix()`.
fn strip_dotgit(p: &Path) -> Vec<u8> {
    let mut bytes = path_bytes(p);
    if bytes.ends_with(b"/.git") {
        bytes.truncate(bytes.len() - 5);
    }
    bytes
}

/// Port of `infer_backlink()`: read the `<id>` out of a `gitdir: …/<id>` file
/// and return `worktrees/<id>` when that administrative directory exists.
fn infer_backlink(common: &Path, gitfile: &Path) -> Option<PathBuf> {
    let actual = std::fs::read(gitfile).ok()?;
    if !actual.starts_with(b"gitdir:") {
        return None;
    }
    let last = actual.iter().rposition(|&b| b == b'/')?;
    let id = rtrim(&actual[last + 1..]);
    if id.is_empty() {
        return None;
    }
    let inferred = common.join("worktrees").join(gix::path::from_byte_slice(id));
    inferred.is_dir().then_some(inferred)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a unique scratch directory under the system temp dir.
    fn scratch(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("zvcs-wt-{tag}-{nonce}-{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // Verified against git worktree.c `should_prune_worktree()`: an
    // administrative directory with no `gitdir` file prunes with this exact
    // reason.
    #[test]
    fn should_prune_missing_gitdir_file() {
        let dir = scratch("nogitdir");
        let admin = dir.join("wt");
        std::fs::create_dir_all(&admin).unwrap();
        match should_prune(&admin, u64::MAX) {
            PruneCheck::Prune(r) => assert_eq!(r, "gitdir file does not exist"),
            _ => panic!("expected prune"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // An empty (all-whitespace) `gitdir` file is `invalid gitdir file`.
    #[test]
    fn should_prune_empty_gitdir_file() {
        let dir = scratch("emptygitdir");
        let admin = dir.join("wt");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("gitdir"), b"\n").unwrap();
        match should_prune(&admin, u64::MAX) {
            PruneCheck::Prune(r) => assert_eq!(r, "invalid gitdir file"),
            _ => panic!("expected prune"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // A `gitdir` file naming a non-existent `.git` entry prunes with the
    // location reason once the (missing) index counts as stale under the default
    // TIME_MAX threshold.
    #[test]
    fn should_prune_dangling_gitdir_target() {
        let dir = scratch("dangling");
        let admin = dir.join("wt");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("gitdir"), b"/no/such/place/.git\n").unwrap();
        match should_prune(&admin, u64::MAX) {
            PruneCheck::Prune(r) => {
                assert_eq!(r, "gitdir file points to non-existent location")
            }
            _ => panic!("expected prune"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // A `gitdir` file whose `.git` target still exists is kept, and the recorded
    // path is surfaced for duplicate detection.
    #[test]
    fn should_keep_live_gitdir_target() {
        let dir = scratch("live");
        let admin = dir.join("wt");
        std::fs::create_dir_all(&admin).unwrap();
        let live = dir.join("checkout.git");
        std::fs::create_dir_all(&live).unwrap();
        let recorded = path_bytes(&live);
        let mut file = recorded.clone();
        file.push(b'\n');
        std::fs::write(admin.join("gitdir"), &file).unwrap();
        match should_prune(&admin, u64::MAX) {
            PruneCheck::Keep(Some(p)) => assert_eq!(p, recorded),
            _ => panic!("expected keep with recorded path"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // read_gitfile classifies a file lacking the `gitdir: ` prefix as an invalid
    // format, and a bare `gitdir: ` with no payload as having no path — the two
    // error codes git's `read_gitfile_gently()` returns before any repo probe.
    #[test]
    fn read_gitfile_format_errors() {
        let dir = scratch("gitfile");
        let bad = dir.join("bad");
        std::fs::write(&bad, b"garbage\n").unwrap();
        assert!(read_gitfile(&bad) == Err(GitfileErr::InvalidFormat));

        let empty = dir.join("empty");
        std::fs::write(&empty, b"gitdir: \n").unwrap();
        assert!(read_gitfile(&empty) == Err(GitfileErr::NoPath));

        let missing = dir.join("missing");
        assert!(read_gitfile(&missing) == Err(GitfileErr::StatFailed));
        std::fs::remove_dir_all(&dir).ok();
    }

    // strbuf_strip_suffix("/.git") and the main-worktree "/." trim.
    #[test]
    fn path_suffix_trims_match_git() {
        assert_eq!(strip_dotgit(Path::new("/a/b/.git")), b"/a/b".to_vec());
        assert_eq!(strip_dotgit(Path::new("/a/b")), b"/a/b".to_vec());
    }
}

// ---------------------------------------------------------------------------
// `git worktree add`
// ---------------------------------------------------------------------------

/// What `add_worktree()` was asked to attach.
enum Start {
    /// Check out an existing branch, attaching the new worktree's `HEAD` to it.
    Branch(FullName, ObjectId),
    /// `-b`/`-B`, or the DWIM branch named after the directory. `from` is the start
    /// point as the caller spelled it (`HEAD` when none was given), which is what the
    /// branch's `branch: Created from <x>` reflog line names.
    NewBranch { name: FullName, oid: ObjectId, force: bool, from: String },
    /// `--detach`, or a commit-ish that is not a branch.
    Detached(ObjectId),
}

impl Start {
    fn oid(&self) -> ObjectId {
        match self {
            Start::Branch(_, oid) | Start::Detached(oid) => *oid,
            Start::NewBranch { oid, .. } => *oid,
        }
    }

    /// The parenthesised text of the `Preparing worktree (…)` line.
    fn preparing(&self, repo: &gix::Repository) -> String {
        match self {
            Start::Branch(name, _) => {
                format!("checking out '{}'", name.as_bstr().to_str_lossy().trim_start_matches("refs/heads/"))
            }
            Start::NewBranch { name, .. } => format!(
                "new branch '{}'",
                name.as_bstr().to_str_lossy().trim_start_matches("refs/heads/")
            ),
            Start::Detached(oid) => format!("detached HEAD {}", abbrev(repo, *oid)),
        }
    }
}

/// `find_unique_abbrev()` for the two messages that carry one.
fn abbrev(repo: &gix::Repository, oid: ObjectId) -> String {
    oid.attach(repo)
        .shorten()
        .map(|p| p.to_string())
        .unwrap_or_else(|_| oid.to_hex_with_len(7).to_string())
}

/// `git worktree add [-f] [--detach] [--[no-]checkout] [(-b|-B) <branch>] <path> [<commit-ish>]`
///
/// Ported from `builtin/worktree.c`'s `add()` / `add_worktree()`. The steps are
/// git's, in git's order, because the messages interleave with them: the
/// `Preparing worktree (…)` line is printed *before* the branch-in-use check, so a
/// refused add still announces what it was about to do.
fn add(args: &[String]) -> Result<ExitCode> {
    let mut new_branch: Option<String> = None;
    let mut force_branch = false;
    let mut detach = false;
    let mut force = false;
    let mut checkout = true;
    let mut quiet = false;
    let mut lock_it = false;
    let mut lock_reason: Option<String> = None;
    let mut positional: Vec<&str> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            // `--help-all` renders `USAGE_FULL`, the same block: no hidden entry.
            s if s == "--help" || super::asks_for_help(s, "fdq") => {
                print!("{ADD_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-b" | "-B" => {
                let Some(v) = args.get(i + 1) else {
                    return usage(Some(&format!("error: switch `{}' requires a value", &a[1..])), ADD_USAGE);
                };
                new_branch = Some(v.clone());
                force_branch = a == "-B";
                i += 1;
            }
            "--detach" => detach = true,
            "-f" | "--force" => force = true,
            "--checkout" => checkout = true,
            "--no-checkout" => checkout = false,
            "-q" | "--quiet" => quiet = true,
            "--lock" => lock_it = true,
            "--reason" => {
                let Some(v) = args.get(i + 1) else {
                    return usage(Some("error: option `reason' requires a value"), ADD_USAGE);
                };
                lock_reason = Some(v.clone());
                i += 1;
            }
            // Refused rather than approximated: each needs behaviour this port
            // does not have — remote-tracking DWIM for the first two, and an
            // unborn HEAD with an empty index for the third.
            "--track" | "--no-track" | "--guess-remote" | "--no-guess-remote" | "--orphan"
            | "--relative-paths" | "--no-relative-paths" => {
                bail!("`worktree add {a}` is not ported")
            }
            // Named as parse-options names it: a long one keeps whatever
            // followed its `=`, a short one is reported as a switch by its first
            // character alone.
            s if s.starts_with('-') && s.len() > 1 => {
                return Ok(super::unknown_option(s, ADD_USAGE))
            }
            s => positional.push(s),
        }
        i += 1;
    }

    let Some(path_arg) = positional.first().copied() else {
        // `if (ac < 1) usage_with_options(...)` — the block alone, no `error:` line.
        return usage(None, ADD_USAGE);
    };
    let commit_ish = positional.get(1).copied();

    let repo = gix::discover(".")?;
    let common = gix::path::realpath(repo.common_dir())?;
    let path = PathBuf::from(path_arg);

    // `add()`: with no `<commit-ish>` and no `-b`, git invents a branch named
    // after the final path component.
    let dwim_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .unwrap_or_default();

    let start = resolve_start(&repo, new_branch.as_deref(), force_branch, detach, commit_ish, &dwim_name)?;

    if !quiet {
        eprintln!("Preparing worktree ({})", start.preparing(&repo));
    }

    // The `-b` branch is created by a child `git branch <name> <commit>`, so an
    // option-shaped name is refused by that child — after the line above has
    // already been printed, and with `git branch`'s usage block. The child's
    // failure surfaces as 255 and nothing is left behind. Without this,
    // `git worktree add -b --zzbogus <path>` created `refs/heads/--zzbogus`
    // *and* a checked-out worktree from a typo.
    if let Start::NewBranch { name, .. } = &start {
        let short = name.as_bstr().to_str_lossy().trim_start_matches("refs/heads/").to_string();
        if let Some(code) = super::branch::child_branch_option_rejection(&repo, &short) {
            return Ok(code);
        }
    }

    // `die_if_checked_out()`: a branch may be checked out in one worktree only.
    if let Start::Branch(name, _) = &start {
        if !force {
            if let Some(other) = checked_out_in(&repo, name)? {
                eprintln!(
                    "fatal: '{}' is already used by worktree at '{}'",
                    name.as_bstr().to_str_lossy().trim_start_matches("refs/heads/"),
                    path_to_string(&other)
                );
                return Ok(ExitCode::from(128));
            }
        }
    }

    // `validate_new_branchname()` runs before a single directory is made, so a
    // `-b` naming an existing branch leaves nothing behind. git reports it through
    // a failed child process, which is why the status is 255 rather than `die()`'s
    // 128.
    if let Start::NewBranch { name, force, .. } = &start {
        if !force && repo.try_find_reference(name.as_bstr())?.is_some() {
            eprintln!(
                "fatal: a branch named '{}' already exists",
                name.as_bstr().to_str_lossy().trim_start_matches("refs/heads/")
            );
            return Ok(ExitCode::from(255));
        }
    }

    // `add_worktree()`: the destination must be absent, or an empty directory.
    let occupied = match std::fs::read_dir(&path) {
        Ok(mut entries) => entries.next().is_some(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        // A plain file at the path is just as occupied as a full directory.
        Err(_) => path.exists(),
    };
    if occupied && !force {
        eprintln!("fatal: '{path_arg}' already exists");
        return Ok(ExitCode::from(128));
    }

    // The administrative directory, named after the path's last component with
    // git's `<name>N` de-duplication when that name is taken.
    let id = unique_admin_id(&common, &dwim_name);
    let admin = common.join("worktrees").join(&id);
    std::fs::create_dir_all(admin.join("logs"))?;
    std::fs::create_dir_all(admin.join("refs"))?;
    std::fs::create_dir_all(&path)?;
    let worktree_abs = gix::path::realpath(&path)?;

    // `<path>/.git` points at the administrative directory, and `gitdir` points
    // back at that file — the two halves `repair` checks against each other.
    std::fs::write(path.join(".git"), format!("gitdir: {}\n", path_to_string(&admin)))?;
    std::fs::write(admin.join("gitdir"), format!("{}\n", path_to_string(&worktree_abs.join(".git"))))?;
    std::fs::write(admin.join("commondir"), "../..\n")?;

    // `-b`/`-B` creates the branch before `HEAD` can name it.
    if let Start::NewBranch { name, oid, force, from } = &start {
        create_branch(&repo, name, *oid, *force, from)?;
    }
    let head_line = match &start {
        Start::Branch(name, _) => format!("ref: {}\n", name.as_bstr().to_str_lossy()),
        Start::NewBranch { name, .. } => format!("ref: {}\n", name.as_bstr().to_str_lossy()),
        Start::Detached(oid) => format!("{}\n", oid.to_hex()),
    };
    std::fs::write(admin.join("HEAD"), head_line)?;
    std::fs::write(admin.join("ORIG_HEAD"), format!("{}\n", start.oid().to_hex()))?;
    write_worktree_reflog(&repo, &admin, start.oid())?;

    if lock_it {
        std::fs::write(admin.join("locked"), lock_reason.map(|r| format!("{r}\n")).unwrap_or_default())?;
    }

    if checkout {
        checkout_into(&repo, &admin, &path, start.oid())?;
    }

    // The `HEAD is now at …` line comes from the checkout itself, so
    // `--no-checkout` has nothing to report.
    if !quiet && checkout {
        let subject = repo
            .find_commit(start.oid())
            .ok()
            .and_then(|c| c.message().ok().map(|m| m.summary().to_string()))
            .unwrap_or_default();
        // `reset --hard`'s line, which the child writes to stdout — unlike the
        // `Preparing worktree` line above it.
        println!("HEAD is now at {} {subject}", abbrev(&repo, start.oid()));
    }
    Ok(ExitCode::SUCCESS)
}

/// Which of `<commit-ish>`, `-b`/`-B` and the DWIM branch this add starts from.
fn resolve_start(
    repo: &gix::Repository,
    new_branch: Option<&str>,
    force_branch: bool,
    detach: bool,
    commit_ish: Option<&str>,
    dwim_name: &str,
) -> Result<Start> {
    let peel = |spec: &str| -> Result<ObjectId> {
        let id = repo
            .rev_parse_single(spec)
            .map_err(|_| anyhow::anyhow!("invalid reference: {spec}"))?;
        Ok(id.object()?.peel_to_commit()?.id)
    };

    if let Some(name) = new_branch {
        let oid = peel(commit_ish.unwrap_or("HEAD"))?;
        return Ok(Start::NewBranch {
            name: FullName::try_from(format!("refs/heads/{name}"))?,
            oid,
            force: force_branch,
            from: commit_ish.unwrap_or("HEAD").to_string(),
        });
    }
    match commit_ish {
        Some(spec) => {
            let oid = peel(spec)?;
            // A `<commit-ish>` that names a branch attaches `HEAD` to it, unless
            // `--detach` asked otherwise.
            let branch = (!detach)
                .then(|| repo.find_reference(spec).ok())
                .flatten()
                .map(|r| r.name().to_owned())
                .filter(|n| n.as_bstr().starts_with(b"refs/heads/"));
            Ok(match branch {
                Some(name) => Start::Branch(name, oid),
                None => Start::Detached(oid),
            })
        }
        // No commit-ish: `-b $(basename <path>)` off HEAD, or a detached HEAD.
        None => {
            let oid = peel("HEAD")?;
            if detach {
                Ok(Start::Detached(oid))
            } else {
                Ok(Start::NewBranch {
                    name: FullName::try_from(format!("refs/heads/{dwim_name}"))?,
                    oid,
                    force: false,
                    from: "HEAD".to_string(),
                })
            }
        }
    }
}

/// `branch_checked_out()` (branch.c): the path of the worktree that holds
/// `refname`, or `None` when no worktree does. `git branch -d` reports it as
/// `used by worktree at '<path>'`, so the path is git's `wt->path` —
/// `get_main_worktree()`'s `real_path(get_git_common_dir())` with a trailing
/// `/.git` cut off for the main worktree, and the (realpath'd, `/.git`-stripped)
/// content of `worktrees/<id>/gitdir` for a linked one. Both are absolute
/// regardless of how the repository was discovered, which is what `collect()`
/// already computes for `git worktree list`.
///
/// `prepare_checked_out_branches()` fills the map from three sources per
/// worktree — the branch `HEAD` names, the branch an interrupted rebase will
/// return to, and the branch a bisect started from — and skips a bare worktree
/// entirely, which is why deleting the branch a bare repository's `HEAD` names
/// is allowed. Later worktrees overwrite earlier ones for the same branch
/// (`strmap_put()`), so the last match wins.
///
/// Not covered: the sequencer's `update-refs` state, git's fourth source, which
/// would need `rebase --update-refs`' file format parsed.
pub(super) fn branch_checked_out(repo: &gix::Repository, refname: &str) -> Result<Option<PathBuf>> {
    let common = gix::path::realpath(repo.common_dir())?;
    let mut found = None;
    for wt in collect(repo, u64::MAX)? {
        if wt.is_bare {
            continue;
        }
        // `get_worktree_git_dir()`: the common dir for the main worktree, the
        // administrative directory for a linked one.
        let wt_gitdir = match &wt.id {
            Some(id) => common.join("worktrees").join(id),
            None => common.clone(),
        };

        if let HeadInfo::Branch { name, .. } = &wt.head {
            if name.as_bstr() == refname {
                found = Some(wt.path.clone());
            }
        }
        // `wt_status_check_rebase()`: `rebase-apply` without `applying` is a
        // rebase (with it, an `am`, which records no branch), `rebase-merge` is
        // one either way. Both keep the original branch in `head-name`.
        let rebase_head_name = if wt_gitdir.join("rebase-apply").is_dir() {
            (!wt_gitdir.join("rebase-apply/applying").exists())
                .then(|| wt_gitdir.join("rebase-apply/head-name"))
        } else if wt_gitdir.join("rebase-merge").is_dir() {
            Some(wt_gitdir.join("rebase-merge/head-name"))
        } else {
            None
        };
        if let Some(branch) = rebase_head_name.and_then(|p| state_branch(&p)) {
            if format!("refs/heads/{branch}") == refname {
                found = Some(wt.path.clone());
            }
        }
        // `wt_status_check_bisect()`: `BISECT_LOG` marks a bisect in progress and
        // `BISECT_START` names what it started from.
        if wt_gitdir.join("BISECT_LOG").exists() {
            if let Some(branch) = state_branch(&wt_gitdir.join("BISECT_START")) {
                if format!("refs/heads/{branch}") == refname {
                    found = Some(wt.path.clone());
                }
            }
        }
    }
    Ok(found)
}

/// `get_branch()` (wt-status.c): read a state file naming a branch, trim its
/// trailing newlines and shorten a `refs/heads/` target. An empty file, or the
/// `detached HEAD` a rebase off a detached `HEAD` records, names no branch;
/// anything else (another `refs/` ref, an object id, a bisect's free-form start)
/// is returned as-is, which simply fails to match a `refs/heads/` name later.
fn state_branch(path: &Path) -> Option<String> {
    let raw = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(raw.as_slice())
        .trim_end_matches('\n')
        .to_owned();
    if text.is_empty() || text == "detached HEAD" {
        return None;
    }
    Some(text.strip_prefix("refs/heads/").unwrap_or(&text).to_owned())
}

/// The worktree whose `HEAD` already points at `branch`, if any — git's
/// `find_shared_symref()`, which is what `die_if_checked_out()` reports.
fn checked_out_in(repo: &gix::Repository, branch: &FullName) -> Result<Option<PathBuf>> {
    for wt in collect(repo, u64::MAX)? {
        if let HeadInfo::Branch { name, .. } = &wt.head {
            if name.as_bstr() == branch.as_bstr() {
                return Ok(Some(wt.path));
            }
        }
    }
    Ok(None)
}

/// `refs/heads/<name>` at `oid`, refusing to clobber unless `-B` was given.
fn create_branch(
    repo: &gix::Repository,
    name: &FullName,
    oid: ObjectId,
    force: bool,
    from: &str,
) -> Result<()> {
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    let expected = if force { PreviousValue::Any } else { PreviousValue::MustNotExist };
    let edit = RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("branch: Created from {from}").into(),
            },
            expected,
            new: gix::refs::Target::Object(oid),
        },
        name: name.clone(),
        deref: false,
    };
    repo.edit_references([edit]).map_err(|e| {
        anyhow::anyhow!(
            "a branch named '{}' already exists: {e}",
            name.as_bstr().to_str_lossy().trim_start_matches("refs/heads/")
        )
    })?;
    Ok(())
}

/// The two `logs/HEAD` lines `add_worktree()` leaves behind: the creation of the
/// new `HEAD`, then the checkout that follows it.
fn write_worktree_reflog(repo: &gix::Repository, admin: &Path, oid: ObjectId) -> Result<()> {
    let now = gix::date::Time::now_local_or_utc().format_or_unix(gix::date::time::Format::Raw);
    let sig = match repo.committer() {
        Some(Ok(sig)) => sig,
        _ => gix::actor::SignatureRef {
            name: b"zvcs".as_bstr(),
            email: b"zvcs@localhost".as_bstr(),
            time: &now,
        },
    };
    let sig = format!("{} <{}> {}", sig.name, sig.email, sig.time);
    let zero = ObjectId::null(repo.object_hash());
    let text = format!(
        "{} {} {}\t\n{} {} {}\treset: moving to HEAD\n",
        zero.to_hex(),
        oid.to_hex(),
        sig,
        oid.to_hex(),
        oid.to_hex(),
        sig
    );
    std::fs::write(admin.join("logs").join("HEAD"), text)?;
    Ok(())
}

/// `<name>`, or `<name>1`, `<name>2`, … when that administrative directory is
/// taken — `add()`'s `worktree_id` loop.
fn unique_admin_id(common: &Path, base: &str) -> String {
    let base = if base.is_empty() { "worktree" } else { base };
    let dir = common.join("worktrees");
    if !dir.join(base).exists() {
        return base.to_owned();
    }
    for n in 1u32.. {
        let candidate = format!("{base}{n}");
        if !dir.join(&candidate).exists() {
            return candidate;
        }
    }
    unreachable!("the loop returns")
}

/// Populate the new worktree: build its index from the commit's tree, write it as
/// the administrative `index`, and lay the files down.
fn checkout_into(repo: &gix::Repository, admin: &Path, path: &Path, oid: ObjectId) -> Result<()> {
    let tree = repo.find_commit(oid)?.tree_id()?.detach();
    let mut index = repo.index_from_tree(&tree)?;
    let opts = repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    let odb = repo.objects.clone().into_arc()?;
    crate::worktree::checkout_subset(
        &mut index,
        path,
        odb,
        &gix::progress::Discard,
        &gix::progress::Discard,
        &std::sync::atomic::AtomicBool::default(),
        opts,
    )?;
    index.write_to(
        std::fs::File::create(admin.join("index"))?,
        gix::index::write::Options::default(),
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// remove / move
// ---------------------------------------------------------------------------

/// `git worktree remove [-f] <worktree>` — port of `remove_worktree()`.
///
/// The checkout goes first and its administrative directory second, so an
/// interrupted removal leaves a prunable entry rather than a live worktree with no
/// bookkeeping. A locked worktree needs `-f -f`, and a dirty one `-f`: git counts
/// the `--force`s and treats the second as "override the lock too".
fn remove(args: &[String]) -> Result<ExitCode> {
    let mut force = 0usize;
    let mut target: Option<&str> = None;
    for a in args {
        match a.as_str() {
            // `--help-all` renders `USAGE_FULL`, the same block: no hidden entry.
            s if s == "--help" || super::asks_for_help(s, "f") => {
                print!("{REMOVE_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-f" | "--force" => force += 1,
            "--no-force" => force = 0,
            s if s.starts_with('-') && s != "-" => return Ok(super::unknown_option(s, REMOVE_USAGE)),
            s if target.is_none() => target = Some(s),
            _ => return usage(None, REMOVE_USAGE),
        }
    }
    let Some(arg) = target else {
        return usage(None, REMOVE_USAGE);
    };

    let repo = gix::discover(".")?;
    let worktrees = collect(&repo, u64::MAX)?;
    let Some(wt) = find_worktree(&worktrees, arg) else {
        return die(&format!("'{arg}' is not a working tree"));
    };
    let Some(id) = wt.id.clone() else {
        return die(&format!("'{arg}' is a main working tree"));
    };
    if force < 2 {
        if let Some(reason) = &wt.locked {
            return die(&if reason.is_empty() {
                "cannot remove a locked working tree;\nuse 'remove -f -f' to override or unlock first"
                    .to_string()
            } else {
                format!(
                    "cannot remove a locked working tree, lock reason: {reason}\nuse 'remove -f -f' to override or unlock first"
                )
            });
        }
    }

    // `check_clean_worktree()`: git runs `status --porcelain` inside the worktree and
    // refuses on any output at all, tracked or not. A checkout that is already gone
    // (`WT_VALIDATE_WORKTREE_MISSING_OK`) skips straight to the bookkeeping.
    if wt.path.exists() {
        if force == 0 {
            if let Some(dirty) = worktree_is_dirty(&wt.path)? {
                if dirty {
                    return die(&format!(
                        "'{arg}' contains modified or untracked files, use --force to delete it"
                    ));
                }
            }
        }
        if let Err(e) = std::fs::remove_dir_all(&wt.path) {
            return die(&format!(
                "failed to delete '{}': {}",
                path_to_string(&wt.path),
                errno_str(&e)
            ));
        }
    }
    let admin = repo.common_dir().join("worktrees").join(&id);
    if let Err(e) = std::fs::remove_dir_all(&admin) {
        return die(&format!(
            "failed to delete '{}': {}",
            path_to_string(&admin),
            errno_str(&e)
        ));
    }
    Ok(ExitCode::SUCCESS)
}

/// `git worktree move <worktree> <new-path>` — port of `move_worktree()`.
///
/// The directory is renamed first, then the two files that point at each other are
/// rewritten: `worktrees/<id>/gitdir` (which names the checkout's `.git` file) and
/// the checkout's own `.git` file (which names the administrative directory).
fn move_worktree(args: &[String]) -> Result<ExitCode> {
    let mut force = false;
    let mut positionals: Vec<&str> = Vec::new();
    for a in args {
        match a.as_str() {
            // `--help-all` renders `USAGE_FULL`, the same block: no hidden entry.
            s if s == "--help" || super::asks_for_help(s, "f") => {
                print!("{MOVE_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-f" | "--force" => force = true,
            "--no-force" => force = false,
            s if s.starts_with('-') && s != "-" => return Ok(super::unknown_option(s, MOVE_USAGE)),
            s => positionals.push(s),
        }
    }
    if positionals.len() != 2 {
        return usage(None, MOVE_USAGE);
    }
    let (arg, dest_arg) = (positionals[0], positionals[1]);

    let repo = gix::discover(".")?;
    let worktrees = collect(&repo, u64::MAX)?;
    let Some(wt) = find_worktree(&worktrees, arg) else {
        return die(&format!("'{arg}' is not a working tree"));
    };
    let Some(id) = wt.id.clone() else {
        return die(&format!("'{arg}' is a main working tree"));
    };
    if !force {
        if let Some(reason) = &wt.locked {
            return die(&if reason.is_empty() {
                "cannot move a locked working tree;\nuse 'move -f -f' to override or unlock first"
                    .to_string()
            } else {
                format!(
                    "cannot move a locked working tree, lock reason: {reason}\nuse 'move -f -f' to override or unlock first"
                )
            });
        }
    }

    // `git worktree move a b` where `b` is a directory moves the checkout *into* it,
    // keeping its own name — the same rule `mv` follows.
    let mut dest = PathBuf::from(dest_arg);
    if dest.is_dir() {
        if let Some(name) = wt.path.file_name() {
            dest = dest.join(name);
        }
    }
    if dest.exists() {
        return die(&format!("target '{dest_arg}' already exists"));
    }
    if let Err(e) = std::fs::rename(&wt.path, &dest) {
        return die(&format!(
            "failed to move '{}' to '{}': {}",
            path_to_string(&wt.path),
            path_to_string(&dest),
            errno_str(&e)
        ));
    }

    // `update_worktree_location()`: both halves of the link are rewritten, absolute,
    // exactly as `worktree add` wrote them.
    let admin = repo.common_dir().join("worktrees").join(&id);
    let dest_abs = gix::path::realpath(&dest).unwrap_or(dest.clone());
    let mut gitdir_line = path_bytes(&dest_abs.join(".git"));
    gitdir_line.push(b'\n');
    std::fs::write(admin.join("gitdir"), gitdir_line)?;
    let admin_abs = gix::path::realpath(&admin).unwrap_or_else(|_| admin.clone());
    let mut dot_git = b"gitdir: ".to_vec();
    dot_git.extend_from_slice(&path_bytes(&admin_abs));
    dot_git.push(b'\n');
    std::fs::write(dest_abs.join(".git"), dot_git)?;
    Ok(ExitCode::SUCCESS)
}

/// `check_clean_worktree()`'s question, asked of the worktree at `path`: does
/// `status --porcelain` have anything to say? `None` when the checkout cannot be
/// opened at all, which git treats as nothing to protect.
fn worktree_is_dirty(path: &Path) -> Result<Option<bool>> {
    let Ok(repo) = gix::open(path) else {
        return Ok(None);
    };
    if repo.is_dirty()? {
        return Ok(Some(true));
    }
    // `is_dirty()` only knows about tracked paths; git refuses over an untracked file
    // just as readily, so the same dirwalk `clean` uses answers the rest.
    let index = repo.index_or_empty()?;
    let options = repo
        .dirwalk_options()?
        .emit_untracked(gix::dir::walk::EmissionMode::CollapseDirectory)
        .emit_ignored(None)
        .emit_empty_directories(false);
    let patterns: Vec<gix::bstr::BString> = Vec::new();
    let iter = repo.dirwalk_iter(index, patterns, Default::default(), options)?;
    for item in iter {
        let item = item?;
        if matches!(item.entry.status, gix::dir::entry::Status::Untracked) {
            return Ok(Some(true));
        }
    }
    Ok(Some(false))
}
