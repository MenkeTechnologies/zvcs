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
/// only the first.
///
/// `--track` / `--no-track` are the `OPT_PASSTHRU` git hands to the child
/// `git branch` (worktree.c:819-821, 946-947), so they are forwarded to
/// [`super::branch::worktree_tracking`] rather than reimplemented: `--track`
/// writes `branch.<n>.remote`/`.merge` and prints
/// `branch '<n>' set up to track '<u>'.` on stdout, `--no-track` suppresses the
/// auto-tracking `branch.autoSetupMerge` would otherwise do. The option carries
/// `PARSE_OPT_NOARG`, so `--track=direct` is `error: option \`track' takes no
/// value` and exit 129, not a mode selector. With nothing to create the passthru
/// has no child to reach, which is `--[no-]track can only be used if a new branch
/// is created` (128) — after the `Preparing worktree` line, and not for the DWIM
/// branch named after the path, which *is* a new branch.
///
/// `--guess-remote` (and `worktree.guessRemote`) reaches the one decision git gives
/// it, `dwim_branch()`'s remote guess for the branch named after the path; it can
/// neither move an explicit `<commit-ish>` nor apply once `-b` named the branch.
/// `--relative-paths` (and `worktree.useRelativePaths`) makes each side name the
/// other relatively. `--orphan` creates the unborn branch, with its option-conflict
/// checks in git's order — the `--track` pair first (worktree.c:839-841), so
/// `--orphan --track` reports `options '--orphan' and '--track' cannot be used
/// together`, naming `--track` whichever spelling was given.
///
/// git creates the `-b` branch by running `git branch` in the new worktree, so
/// an option-shaped name is refused by that child rather than by `worktree`
/// itself; [`super::branch::child_branch_option_rejection`] reproduces what the
/// child says and the status it fails with, which is what keeps
/// `git worktree add -b --zzbogus <path>` from creating a ref and a checkout
/// from a typo.
///
/// `move` and `remove` are ported, `check_clean_worktree()` included: both refuse a
/// linked worktree whose `git status --porcelain` is not empty unless `--force` is
/// given, and `move` rewrites the linking files on both sides
/// (`update_worktree_location()`).
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
    /// `wt->is_current` — `is_current_worktree()` (worktree.c:57-65), which
    /// compares the repository's **git directory** against this entry's:
    ///
    /// ```c
    ///         char *git_dir = absolute_pathdup(repo_get_git_dir(wt->repo));
    ///         char *wt_git_dir = get_worktree_git_dir(wt);
    ///         int is_current = !fspathcmp(git_dir, absolute_path(wt_git_dir));
    /// ```
    ///
    /// Not the checkout path: a repository whose git directory is not `<path>/.git`
    /// — a submodule's `modules/<name>`, or any `--separate-git-dir` — has a main
    /// worktree whose `path` (`realpath(common_dir)` minus a `/.git` suffix) is not
    /// where the checkout lives, so comparing checkouts reports the current
    /// worktree as somebody else. That is what made
    /// `git submodule add -b main ./sub other` die with
    /// `'main' is already used by worktree at '…/.git/modules/other'`.
    is_current: bool,
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
        // gitoxide refuses a `HEAD` git's zero-flag resolve forgives; see
        // [`head_info_of_git_dir`] for which failures are which.
        return head_info_of_git_dir(repo, repo.git_dir());
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

/// `add_head_info()` for a worktree whose ref store would not open at all —
/// an administrative directory holding a `gitdir` file and little else.
///
/// The resolve flags git passes are *zero*, not `RESOLVE_REF_READING`
/// (worktree.c:45-48), and that is the whole difference:
///
/// ```c
///                 /* In reading mode, refs must eventually resolve */
///                 if (resolve_flags & RESOLVE_REF_READING)
///                         return NULL;
///                 /*
///                  * Otherwise a missing ref is OK. …
///                  */
///                 if (failure_errno != ENOENT &&
///                     failure_errno != EISDIR &&
///                     failure_errno != ENOTDIR)
///                         return NULL;
///                 oidclr(oid, refs->repo->hash_algo);
/// ```
/// (refs.c:2144-2160)
///
/// So a `HEAD` that is simply absent resolves *successfully* to the null id with
/// `REF_ISSYMREF` unset, which leaves `wt->is_detached = 1` — and
/// `builtin/worktree.c:1013` prints `(detached HEAD)`, `show_worktree_porcelain`
/// prints the `detached` line. `(error)` is the `else` at builtin/worktree.c:1021
/// and is reached only when the resolve really returned NULL, i.e. the read
/// failed for some *other* reason, or a symref named something `HEAD` may not
/// point at.
fn head_info_of_git_dir(repo: &gix::Repository, git_dir: &Path) -> HeadInfo {
    let null = ObjectId::null(repo.object_hash());
    let raw = match std::fs::read(git_dir.join("HEAD")) {
        Ok(raw) => raw,
        Err(e) => {
            // The three errnos refs.c:2153-2155 forgives. Everything else is the
            // NULL return, i.e. `(error)`.
            let forgiven = e.kind() == std::io::ErrorKind::NotFound
                || matches!(e.raw_os_error(), Some(libc::EISDIR) | Some(libc::ENOTDIR));
            return if forgiven { HeadInfo::Detached(null) } else { HeadInfo::Unknown };
        }
    };
    let text = String::from_utf8_lossy(rtrim(&raw)).into_owned();
    match text.strip_prefix("ref:") {
        // A symref whose target does not exist still resolves: the loop's second
        // pass forgives the missing ref and `*flags` keeps the `REF_ISSYMREF` the
        // first pass set, so the worktree shows the branch name at the null id.
        Some(target) => match FullName::try_from(target.trim().to_owned()) {
            Ok(name) => {
                let oid = repo
                    .find_reference(name.as_ref())
                    .ok()
                    .and_then(|r| r.target().try_id().map(ObjectId::from))
                    .unwrap_or(null);
                HeadInfo::Branch { oid, name }
            }
            Err(_) => HeadInfo::Unknown,
        },
        None => ObjectId::from_hex(text.as_bytes()).map_or(HeadInfo::Unknown, HeadInfo::Detached),
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
    // `is_current_worktree()` (worktree.c:57-65) compares git directories, and
    // `get_worktree_git_dir()` (:437-445) is the common directory for the main
    // worktree and `worktrees/<id>` for a linked one.
    let this_git_dir = gix::path::realpath(repo.git_dir()).unwrap_or_else(|_| repo.git_dir().to_owned());
    let is_bare = repo.is_bare();
    // `add_head_info()` reads each entry's `HEAD` from *that worktree's* ref store
    // (worktree.c:46), and the main worktree's is the common one. Reading the current repository's
    // `HEAD` instead reports the main worktree as sitting on the branch of whichever linked
    // worktree the command was run from.
    let main_head = if is_bare {
        HeadInfo::Unknown
    } else {
        match gix::open(&common) {
            Ok(main) => head_info(&main),
            Err(_) => head_info(repo),
        }
    };
    let mut out = vec![Wt {
        path: main_path,
        id: None,
        is_bare,
        head: main_head,
        locked: None,
        prunable: None,
        is_current: this_git_dir == common,
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
        // The file names the worktree's `.git` entry. `should_prune_worktree()` tests it for
        // existence after resolving a *relative* recording against the administrative directory
        // (worktree.c:995) — the spelling `worktree add --relative-paths` and
        // `worktree.useRelativePaths` write.
        let dot_git = PathBuf::from(String::from_utf8_lossy(trimmed).into_owned());
        let missing = !recorded_dot_git(&admin, trimmed).exists();

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
                Err(_) => head_info_of_git_dir(repo, &admin),
            },
            None => head_info_of_git_dir(repo, &admin),
        };

        linked.push(Wt {
            path,
            id: Some(id),
            is_bare: false,
            head,
            locked,
            prunable,
            is_current: this_git_dir
                == gix::path::realpath(&admin).unwrap_or_else(|_| admin.clone()),
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
                    return Ok(crate::parseopt::requires_value(crate::parseopt::OptName::Long("expire")));
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

    let repo = crate::setup::discover()?;
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
                    return Ok(crate::parseopt::requires_value(crate::parseopt::OptName::Long(
                        "reason",
                    )));
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

    let repo = crate::setup::discover()?;
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

    let repo = crate::setup::discover()?;
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
                    return Ok(crate::parseopt::requires_value(crate::parseopt::OptName::Long("expire")));
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

    let repo = crate::setup::discover()?;
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
///
/// Shared with `gc`, which runs `git worktree prune --expire <gc.worktreePruneExpire>`.
pub(super) fn prune_worktrees(repo: &gix::Repository, show_only: bool, verbose: bool, expire: u64) {
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

/// Whether `should_prune_worktree()` would prune this administrative directory — the
/// question `gc`'s and `maintenance`'s auto-conditions ask without doing the pruning.
pub(super) fn is_prunable(admin: &Path, expire: u64) -> bool {
    matches!(should_prune(admin, expire), PruneCheck::Prune(_))
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
    // git hands the *resolved* `.git` path back through `*wtpath`, so a relative recording takes
    // part in dup detection under the same name an absolute one would (worktree.c:995-1010).
    let target = recorded_dot_git(admin, recorded);
    let resolved = gix::path::into_bstr(target.as_path()).into_owned().into();
    if target.exists() {
        return PruneCheck::Keep(Some(resolved));
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
        PruneCheck::Keep(Some(resolved))
    }
}

/// `should_prune_worktree()`'s reading of a `worktrees/<id>/gitdir` file (worktree.c:995): an
/// absolute recording stands as it is, a relative one is joined onto the realpath of the
/// administrative directory and resolved as far as it can be. `worktree add --relative-paths` and
/// `worktree.useRelativePaths` write the relative spelling, and testing that string for existence
/// from wherever the process happens to stand reports a healthy worktree as prunable.
pub(super) fn recorded_dot_git(admin: &Path, recorded: &[u8]) -> PathBuf {
    let path = gix::path::from_byte_slice(recorded);
    if path.is_absolute() {
        return path.to_owned();
    }
    let base = gix::path::realpath(admin).unwrap_or_else(|_| admin.to_owned());
    let joined = base.join(path);
    gix::path::realpath(&joined).unwrap_or(joined)
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

    let repo = crate::setup::discover()?;
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
    // `strbuf_realpath(&realdotgit, realdotgit.buf, 0)` tolerates a missing
    // *last* component and nothing before it, so a `<path>` that is not there at
    // all — a typo, or a worktree whose directory has been deleted — fails the
    // resolution and is reported as "not a valid path" rather than reaching the
    // gitfile read and being reported as a repository that could not be located.
    // `gix::path::realpath` resolves the missing parent happily, so the parent is
    // tested here.
    let realdotgit = match gix::path::realpath(&dotgit) {
        Ok(r) if std::fs::metadata(path).is_ok() => r,
        // strbuf_realpath(die_on_error=0): reported, not fatal.
        _ => {
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
    /// `--orphan`: `HEAD` names a branch that does not exist yet, and nothing is checked out.
    /// git creates no ref here — the branch comes into being with the worktree's first commit.
    Orphan(FullName),
}

impl Start {
    fn oid(&self) -> ObjectId {
        match self {
            Start::Branch(_, oid) | Start::Detached(oid) => *oid,
            Start::NewBranch { oid, .. } => *oid,
            // An unborn `HEAD` has no commit; every use of this value is skipped for `Orphan`.
            Start::Orphan(_) => ObjectId::null(gix::hash::Kind::Sha1),
        }
    }

    /// The parenthesised text of the `Preparing worktree (…)` line.
    fn preparing(&self, repo: &gix::Repository) -> String {
        match self {
            Start::Branch(name, _) => {
                format!("checking out '{}'", name.as_bstr().to_str_lossy().trim_start_matches("refs/heads/"))
            }
            // `print_preparing_worktree_line()`: `-B` looks the branch up, and an existing one is
            // announced as a reset naming the commit it is about to leave
            // (builtin/worktree.c:640-647).
            Start::NewBranch { name, force, .. } => {
                let short = name.as_bstr().to_str_lossy().trim_start_matches("refs/heads/").to_string();
                let existing = force
                    .then(|| repo.try_find_reference(name.as_ref()).ok().flatten())
                    .flatten()
                    .and_then(|mut r| r.peel_to_id_in_place().ok().map(|id| id.detach()));
                match existing {
                    Some(oid) => format!("resetting branch '{short}'; was at {}", abbrev(repo, oid)),
                    None => format!("new branch '{short}'"),
                }
            }
            Start::Detached(oid) => format!("detached HEAD {}", abbrev(repo, *oid)),
            // `--orphan` reaches `print_preparing_worktree_line()` with `new_branch` set and no
            // force, so it prints the same line `-b` does (builtin/worktree.c:648).
            Start::Orphan(name) => format!(
                "new branch '{}'",
                name.as_bstr().to_str_lossy().trim_start_matches("refs/heads/")
            ),
        }
    }
}

/// Port of `worktree_basename()` (builtin/worktree.c:296): trailing separators are dropped and
/// what follows the last remaining one is the name. It is plain text, not a path component —
/// `worktree add .` therefore asks for a branch called `.`, which the `git branch` child refuses,
/// where `Path::file_name()` answers `None` and would silently ask for an empty name.
fn worktree_basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches(std::path::is_separator);
    match trimmed.rfind(std::path::is_separator) {
        Some(sep) => &trimmed[sep + 1..],
        None => trimmed,
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
    // git keeps `-b` and `-B` in two `OPT_STRING` slots (`new_branch` and
    // `new_branch_force`) and only merges them after the conflict check below, so
    // whether each *spelling* was seen is what that check reads — `-b x -b y` is
    // last-one-wins while `-b x -B y` is fatal.
    let mut saw_new_branch = false;
    let mut saw_new_branch_force = false;
    let mut detach = false;
    let mut force = false;
    let mut checkout = true;
    let mut quiet = false;
    let mut lock_it = false;
    let mut lock_reason: Option<String> = None;
    // `--track` / `--no-track`; `None` when neither was given.
    let mut opt_track: Option<bool> = None;
    let mut orphan = false;
    // `worktree.guessRemote`, which `--[no-]guess-remote` then overrides
    // (worktree.c:131, :138-139, :823).
    let mut guess_remote: Option<bool> = None;
    // `--[no-]relative-paths` over `worktree.useRelativePaths`, resolved once the repository is
    // open below.
    let mut relative_paths: Option<bool> = None;
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
                    return Ok(crate::parseopt::requires_value(crate::parseopt::OptName::Short(a[1..].chars().next().unwrap_or('-'))));
                };
                new_branch = Some(v.clone());
                force_branch = a == "-B";
                if force_branch {
                    saw_new_branch_force = true;
                } else {
                    saw_new_branch = true;
                }
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
                    return Ok(crate::parseopt::requires_value(crate::parseopt::OptName::Long("reason")));
                };
                lock_reason = Some(v.clone());
                i += 1;
            }
            // `OPT_PASSTHRU(0, "track", …, PARSE_OPT_NOARG | PARSE_OPT_OPTARG)`
            // (worktree.c:819-821): the spelling is captured and pushed onto the
            // child `git branch`'s command line verbatim. `NOARG` is what makes
            // `--track=direct` an error rather than a mode selector, so only the
            // two bare forms reach the child.
            "--track" => opt_track = Some(true),
            "--no-track" => opt_track = Some(false),
            s if s.starts_with("--track=") => {
                // parse-options answers this one itself, with no usage block.
                eprintln!("error: option `track' takes no value");
                return Ok(ExitCode::from(129));
            }
            // Deferred rather than refused here: git checks `--orphan` against
            // `--track` first (worktree.c:839-841) and reports the *combination*,
            // so the refusal for `--orphan` alone has to wait until both are known.
            "--orphan" => orphan = true,
            // `OPT_BOOL(0, "guess-remote", &guess_remote, …)` (worktree.c:823)
            // writes the same variable `worktree.guessRemote` initialised, so
            // the command line is simply the last word.
            "--guess-remote" => guess_remote = Some(true),
            "--no-guess-remote" => guess_remote = Some(false),
            // `OPT_BOOL(0, "relative-paths", &opts.relative_paths, …)` (worktree.c:824), which
            // `worktree.useRelativePaths` initialises.
            "--relative-paths" => relative_paths = Some(true),
            "--no-relative-paths" => relative_paths = Some(false),
            // `add()` calls `parse_options()` without `PARSE_OPT_KEEP_DASHDASH`,
            // so `--` is consumed and everything after it is a non-option
            // argument. Three of those is `ac > 2`, which is the usage block —
            // not an unknown option named by the empty string.
            "--" => {
                positional.extend(args[i + 1..].iter().map(String::as_str));
                break;
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

    // `if (!!opts.detach + !!new_branch + !!new_branch_force > 1)` (worktree.c:836),
    // the very first check `add()` makes — ahead of every `--orphan` combination,
    // measured against git 2.50.1: `worktree add --orphan --detach -b x p` reports
    // this one, not the `--orphan`/`--detach` pair below.
    if u8::from(detach) + u8::from(saw_new_branch) + u8::from(saw_new_branch_force) > 1 {
        eprintln!("fatal: options '-b', '-B', and '--detach' cannot be used together");
        return Ok(ExitCode::from(128));
    }
    // worktree.c:839-841, ahead of every other check and of the `Preparing
    // worktree` line: the combination is reported even when `--no-track` was the
    // spelling given, because the message names the option, not the mode.
    if orphan && opt_track.is_some() {
        eprintln!("fatal: options '--orphan' and '--track' cannot be used together");
        return Ok(ExitCode::from(128));
    }
    // worktree.c:836-847, in git's order and with git's wording. `--detach` is checked before
    // `--track` (which was answered above), then `--no-checkout`, then a `<commit-ish>`: an
    // unborn branch has nothing to detach from, nothing to withhold from the worktree, and no
    // start point to accept.
    if orphan && detach {
        eprintln!("fatal: options '--orphan' and '--detach' cannot be used together");
        return Ok(ExitCode::from(128));
    }
    if orphan && !checkout {
        eprintln!("fatal: options '--orphan' and '--no-checkout' cannot be used together");
        return Ok(ExitCode::from(128));
    }
    if orphan && positional.len() == 2 {
        eprintln!("fatal: option '--orphan' and commit-ish cannot be used together");
        return Ok(ExitCode::from(128));
    }

    // `if (ac < 1 || ac > 2) usage_with_options(…)` (worktree.c:643) — the block
    // alone, no `error:` line, for either side of the range.
    if positional.len() > 2 {
        return usage(None, ADD_USAGE);
    }
    let Some(path_arg) = positional.first().copied() else {
        return usage(None, ADD_USAGE);
    };
    let commit_ish = positional.get(1).copied();

    let repo = crate::setup::discover()?;
    let common = gix::path::realpath(repo.common_dir())?;
    let path = PathBuf::from(path_arg);

    // `add()`: with no `<commit-ish>` and no `-b`, git invents a branch named
    // after the final path component.
    let dwim_name = worktree_basename(path_arg).to_owned();

    // `add()` puts the `<commit-ish>` through `lookup_commit_reference_by_name()`
    // — and so through `get_oid_basic()` — at four points, each of which warns:
    // builtin/worktree.c:904 (the `ac == 2` arm, which `--detach` skips), :918,
    // `print_preparing_worktree_line()`:641/:657, and `add_worktree()`:490. Stock
    // prints `warning: refname 'dup' is ambiguous.` three times for
    // `git worktree add <path> dup`, whichever of `--detach`/`-b` is in play.
    // ```c
    // if (new_branch_force) {
    //         new_branch = new_branch_force;
    //         if (!opts.force && !strbuf_check_branch_ref(&symref, new_branch) &&
    //             ref_exists(symref.buf))
    //                 die_if_checked_out(symref.buf, 0);
    // }
    // ```
    // worktree.c:659-666 — `-B` is the one spelling that would *reset* a branch
    // another worktree has checked out, so it is refused here, before the DWIM
    // lookups and before `print_preparing_worktree_line()`. `-b` needs no such
    // check: it refuses an existing branch outright.
    if force_branch && !force {
        if let Some(name) = new_branch.as_deref() {
            if let Ok(full) = FullName::try_from(format!("refs/heads/{name}")) {
                if repo.try_find_reference(full.as_bstr()).ok().flatten().is_some() {
                    if let Some(other) = checked_out_in(&repo, &full)? {
                        eprintln!(
                            "fatal: '{name}' is already used by worktree at '{}'",
                            path_to_string(&other)
                        );
                        return Ok(ExitCode::from(128));
                    }
                }
            }
        }
    }

    let branch_arg = commit_ish.unwrap_or("HEAD");
    if commit_ish.is_some() && !detach {
        crate::objname::warn_ambiguous_refname(&repo, branch_arg);
    }
    crate::objname::warn_ambiguous_refname(&repo, branch_arg);

    // `dwim_branch()` (worktree.c:767-789), the `ac < 2` arm's guess. With no
    // `<commit-ish>`, no `-b` and no `--detach`, git names the new branch after
    // the directory — and if **`worktree.guessRemote`** (or `--guess-remote`) is
    // on and no local branch of that name exists, it starts that branch from the
    // one remote-tracking branch whose name matches, making it the upstream:
    //
    // ```c
    // *new_branch = branchname;
    // if (guess_remote) {
    //         char *remote = unique_tracking_name(*new_branch, &oid, NULL);
    //         return remote;
    // }
    // ```
    //
    // The setting only reaches this one decision. It cannot move an explicit
    // `<commit-ish>` (worktree.c:900-912 DWIMs that one unconditionally) and it
    // cannot apply once `-b` named the branch.
    let guess_remote = guess_remote.unwrap_or_else(|| {
        repo.config_snapshot().boolean("worktree.guessRemote").unwrap_or(false)
    });
    let guessed_start = (guess_remote
        && commit_ish.is_none()
        && new_branch.is_none()
        && !detach
        && !dwim_name.is_empty()
        && repo
            .try_find_reference(format!("refs/heads/{dwim_name}").as_str())
            .ok()
            .flatten()
            .is_none())
    .then(|| unique_tracking_name(&repo, &dwim_name))
    .flatten();

    // worktree.c:919-930, the floor every `add` passes through before anything is
    // created: `if (!opts.orphan && !lookup_commit_reference_by_name(branch))` is
    // `die(_("invalid reference: %s"), branch)`, and `attempt_hint = !opts.quiet &&
    // (ac < 2)` decides whether the `advice.worktreeAddOrphan` hint is offered
    // first. The hint is for the DWIM forms only: with an explicit `<commit-ish>`
    // that does not resolve, the user named something that does not exist rather
    // than reaching for an unborn branch.
    // The branch this add will ask `git branch` to create, if it asks for one at all: `-b`/`-B`,
    // or — with no `<commit-ish>` and no `--detach` — the DWIM name taken from the path. A name
    // the child would refuse is reported by the child, after the `Preparing worktree` line that
    // `add()` prints before running it, so `git worktree add .` announces the branch `.` and only
    // then says it is not a valid one.
    let intended_new_branch = new_branch
        .clone()
        .or_else(|| (commit_ish.is_none() && !detach).then(|| dwim_name.clone()));
    if let Some(name) = intended_new_branch.as_deref() {
        if !super::branch::valid_branch_name(name) {
            if !quiet {
                eprintln!("Preparing worktree (new branch '{name}')");
            }
            if let Some(code) = super::branch::child_branch_invalid_name(&repo, name) {
                return Ok(code);
            }
        }
    }

    // `--orphan` names a branch that does not exist yet — `-b`'s name, or the path's last
    // component (worktree.c:877-882) — and skips the `lookup_commit_reference_by_name()` floor
    // below, which is what `!opts.orphan &&` in the C guard says.
    let orphan_start = if orphan {
        let name = new_branch.clone().unwrap_or_else(|| dwim_name.clone());
        Some(Start::Orphan(FullName::try_from(format!("refs/heads/{name}"))?))
    } else {
        None
    };
    let start = match orphan_start {
        Some(start) => start,
        None => match resolve_start(&repo, new_branch.as_deref(), force_branch, detach, commit_ish, &dwim_name, guessed_start.as_deref()) {
            Ok(start) => start,
            Err(e) => {
                match invalid_reference(&repo, branch_arg, path_arg, new_branch.as_deref(), quiet, commit_ish.is_none())? {
                    Some(code) => return Ok(code),
                    // `dwim_orphan()` inferred `--orphan` instead of dying, which is
                    // the unborn-worktree floor this port does not build; let the
                    // resolver's own failure stand.
                    None => return Err(e),
                }
            }
        },
    };

    // `print_preparing_worktree_line()` looks a name up only on the two arms that
    // need an id for the message — `-B` (the branch being reset) and the detached
    // one. `-b`, and the "checking out '<branch>'" arm `check_branch_ref()` takes
    // when `refs/heads/<branch>` exists and `--detach` was not asked for, print a
    // name they already have:
    //
    // ```c
    // if (force_new_branch)  { commit = lookup_commit_reference_by_name(new_branch); … }
    // else if (new_branch)   { … }
    // else {
    //         if (!detach && !check_branch_ref(&s, branch) && refs_ref_exists(…, s.buf))
    //                 … /* "checking out '%s'" */
    //         else { commit = lookup_commit_reference_by_name(branch); … }
    // }
    // ```
    // `check_branch_ref()` is `strbuf_check_branch_ref()`, which *returns* non-zero
    // for a name `refs/heads/<name>` may not be built from — the `!check_branch_ref(…)`
    // guard then simply falls through to the detached arm. A `<commit-ish>` that is a
    // revision expression (`HEAD~1`, `main@{1}`) is exactly that case, so the invalid
    // ref name has to answer "no branch", never propagate as an error.
    let branch_ref_exists = repo
        .try_find_reference(format!("refs/heads/{branch_arg}").as_str())
        .ok()
        .flatten()
        .is_some();
    if force_branch {
        if let Some(name) = new_branch.as_deref() {
            crate::objname::warn_ambiguous_refname(&repo, name);
        }
    } else if new_branch.is_none() && (detach || !branch_ref_exists) {
        crate::objname::warn_ambiguous_refname(&repo, branch_arg);
    }

    if !quiet {
        eprintln!("Preparing worktree ({})", start.preparing(&repo));
    }

    // worktree.c:951-952. The `--[no-]track` passthru only means anything to the
    // child `git branch`, so with no branch to create there is nothing to hand it
    // to. This sits *after* the `Preparing worktree` line, which is where stock
    // prints it — the DWIM branch named after the path counts as a new branch, so
    // this fires only when a `<commit-ish>` named an existing branch or a commit.
    if opt_track.is_some() && !matches!(start, Start::NewBranch { .. }) {
        eprintln!("fatal: --[no-]track can only be used if a new branch is created");
        return Ok(ExitCode::from(128));
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
        // That child is `git branch <new> <branch>`, so `create_branch()` →
        // `dwim_branch_start()` (branch.c:552,562-582) resolves the start-point
        // once more — another `warning: refname … is ambiguous.` — and refuses a
        // name more than one ref answers to. `run_command()` failing here is
        // `return -1`, which `git` reports as 255.
        crate::objname::warn_ambiguous_refname(&repo, branch_arg);
        if super::rev_parse::dwim_ref_matches(&repo, branch_arg).len() > 1 {
            eprintln!("fatal: ambiguous object name: '{branch_arg}'");
            return Ok(ExitCode::from(255));
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

    // `-b`/`-B` creates the branch through a child `git branch` in `add()` (worktree.c:936-950),
    // which runs *before* `add_worktree()` looks at the destination — so a `worktree add <name>`
    // that dies on an occupied path still leaves `refs/heads/<name>` behind, and repeating the
    // command then fails on the branch instead.
    if let Start::NewBranch { name, oid, force, from } = &start {
        create_branch(&repo, name, *oid, *force, from)?;
        // The child `git branch <new> <start> [<opt_track>]` sets the upstream after
        // it writes the ref, and its `branch '<n>' set up to track '<u>'.` lands on
        // stdout between the `Preparing worktree` line and the checkout's
        // `HEAD is now at …`. `super::branch` owns the decision so this cannot
        // drift from `git branch`'s own.
        //
        // Unconditional, not gated on `opt_track`: with no passthru the child still
        // runs, and `branch.autoSetupMerge = always` makes it track the start point.
        // Skipping this when no flag was given left `worktree add -b` under that
        // config with no upstream where git writes one.
        {
            let track = opt_track;
            let short = name.as_bstr().to_str_lossy().trim_start_matches("refs/heads/").to_string();
            // The start point as a ref, which is what tracking is derived from:
            // `HEAD` resolves through the symref, anything else has to name a ref
            // for there to be an upstream at all.
            let start_ref: Option<gix::bstr::BString> = if from == "HEAD" {
                repo.head_name()?.map(|n| n.as_bstr().to_owned())
            } else {
                repo.try_find_reference(from.as_str())
                    .ok()
                    .flatten()
                    .map(|r| r.name().as_bstr().to_owned())
            };
            if let Some(code) = super::branch::worktree_tracking(
                &repo,
                &short,
                start_ref.as_ref().map(|b| b.as_ref()),
                from,
                track,
                quiet,
            )? {
                return Ok(code);
            }
        }
    }

    // `add_worktree()`: the destination must be absent, or an empty directory.
    let occupied = match std::fs::read_dir(&path) {
        Ok(mut entries) => entries.next().is_some(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        // A plain file at the path is just as occupied as a full directory.
        Err(_) => path.exists(),
    };
    // `check_candidate_path()`'s first line is not gated by `--force`:
    //
    // ```c
    // if (file_exists(path) && !is_empty_dir(path))
    //         die(_("'%s' already exists"), path);
    // ```
    //
    // `--force` only overrides the *registered worktree* checks below it, so
    // `git worktree add -f <non-empty-dir>` dies in stock 2.55.0 too.
    if occupied {
        eprintln!("fatal: '{path_arg}' already exists");
        return Ok(ExitCode::from(128));
    }

    // `add_worktree()`:490 — the fourth and last lookup, after
    // `check_candidate_path()` above has had its say.
    crate::objname::warn_ambiguous_refname(&repo, branch_arg);

    // The administrative directory, named after the path's last component with
    // git's `<name>N` de-duplication when that name is taken.
    let id = unique_admin_id(&common, &dwim_name);
    let admin = common.join("worktrees").join(&id);
    std::fs::create_dir_all(admin.join("logs"))?;
    std::fs::create_dir_all(admin.join("refs"))?;
    std::fs::create_dir_all(&path)?;
    let worktree_abs = gix::path::realpath(&path)?;

    // `<path>/.git` points at the administrative directory, and `gitdir` points
    // back at that file — the two halves `repair` checks against each other. With
    // `--relative-paths` (or `worktree.useRelativePaths`) each side names the other relatively and
    // the repository declares `extensions.relativeWorktrees`, which is what
    // `write_worktree_linking_files()` (worktree.c:1092) does for both `add` and `repair`.
    let relative = relative_paths.unwrap_or_else(|| {
        repo.config_snapshot().boolean("worktree.useRelativePaths").unwrap_or(false)
    });
    write_worktree_linking_files(&common, &path.join(".git"), &admin.join("gitdir"), relative);
    std::fs::write(admin.join("commondir"), "../..\n")?;

    let head_line = match &start {
        Start::Branch(name, _) => format!("ref: {}\n", name.as_bstr().to_str_lossy()),
        Start::NewBranch { name, .. } => format!("ref: {}\n", name.as_bstr().to_str_lossy()),
        Start::Detached(oid) => format!("{}\n", oid.to_hex()),
        Start::Orphan(name) => format!("ref: {}\n", name.as_bstr().to_str_lossy()),
    };
    std::fs::write(admin.join("HEAD"), head_line)?;

    // builtin/worktree.c:570-583, in this order and at this point — after `HEAD`
    // exists and before the checkout child runs, because the child reads both
    // files out of the administrative directory it is handed as `GIT_DIR`.
    //
    // ```c
    //      if (cfg->apply_sparse_checkout)
    //              copy_sparse_checkout(sb_repo.buf);
    //      …
    //      if (the_repository->repository_format_worktree_config)
    //              copy_filtered_worktree_config(sb_repo.buf);
    // ```
    //
    // `info/sparse-checkout` is per-worktree (path.c:103 lists it with
    // `is_common = 0`), so without the copy the new worktree has no definition at
    // all and checks the whole tree out — which is what this port used to do.
    let sparsity = super::sparse_checkout::load_sparsity_if_enabled(&repo)?;
    if sparsity.is_some() {
        copy_sparse_checkout(&repo, &admin);
    }
    if repo.config_snapshot().boolean("extensions.worktreeConfig").unwrap_or(false) {
        copy_filtered_worktree_config(&repo, &admin);
    }

    // `--orphan` stops here. `add_worktree()` writes no reflog for it (there is no id to log),
    // `make_worktree_orphan()` only points `HEAD` at the unborn branch, and the `reset --hard`
    // that follows leaves an empty index and an empty worktree — no `ORIG_HEAD`, no
    // `HEAD is now at` line, and no ref: the branch is born with the first commit.
    if let Start::Orphan(_) = start {
        let mut empty = gix::index::State::new(repo.object_hash());
        // The `reset --hard` over an unborn `HEAD` unpacks the empty tree, so its cache-tree names
        // that tree over zero entries. git records the id whether or not the object was ever
        // written — a fresh orphan worktree's index is `DIRC` + `TREE` + `0 0\n` + the empty tree.
        empty.set_tree(Some(gix::index::extension::Tree {
            name: Default::default(),
            id: gix::ObjectId::empty_tree(repo.object_hash()),
            num_entries: Some(0),
            children: Vec::new(),
        }));
        gix::index::File::from_state(empty, admin.join("index")).write_to(
            std::fs::File::create(admin.join("index"))?,
            crate::config::index_write_options(&repo),
        )?;
        if lock_it {
            let reason = lock_reason.unwrap_or_else(|| "added with --lock".to_string());
            std::fs::write(admin.join("locked"), format!("{reason}\n"))?;
        }
        return Ok(ExitCode::SUCCESS);
    }
    // `ORIG_HEAD` and the `reset: moving to HEAD` reflog line are both written by the
    // `reset --hard` child `checkout_worktree()` runs (builtin/worktree.c:400), so `--no-checkout`
    // leaves neither behind. That reset only logs when `HEAD` is symbolic: the branch it
    // dereferences to already names the same commit, so the branch update is a no-op while the
    // symref's own log-only entry is still appended. A detached `HEAD` updates itself to the value
    // it already holds and logs nothing.
    if checkout {
        std::fs::write(admin.join("ORIG_HEAD"), format!("{}\n", start.oid().to_hex()))?;
    }
    let reset_line = checkout && !matches!(start, Start::Detached(_));
    write_worktree_reflog(&repo, &admin, start.oid(), reset_line)?;

    // `add_worktree()` writes `initializing` into `locked` for the duration of the setup and
    // removes it at the end; `--lock` keeps the file, and its content is the `--reason` or, with
    // none, git's own `added with --lock` (builtin/worktree.c:529, :853).
    if lock_it {
        let reason = lock_reason.unwrap_or_else(|| "added with --lock".to_string());
        std::fs::write(admin.join("locked"), format!("{reason}\n"))?;
    }

    if checkout {
        checkout_into(&repo, &admin, &path, start.oid(), sparsity.as_ref())?;
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

/// worktree.c:919-930 — the report for a `<commit-ish>` (or a `HEAD`) that names
/// no commit, and the `advice.worktreeAddOrphan` hint that precedes it.
///
/// `ac_lt_2` is git's `ac < 2`: no `<commit-ish>` was given, so `branch` is the
/// literal `HEAD` and the add is one of the DWIM forms. Those are the only forms
/// that reach `dwim_orphan()` (worktree.c:888-899), and therefore the only ones
/// where "there is nothing to start from" can mean the user wanted an unborn
/// branch. `attempt_hint` is that condition ANDed with `!opts.quiet`, and
/// `used_new_branch_options` (`new_branch || new_branch_force`) picks between the
/// two hint texts so the suggested command keeps the `-b <name>` the user typed.
///
/// Returns `Ok(None)` when git would not have died here at all: `dwim_orphan()`
/// infers `--orphan` when the repository has no local refs whatsoever, so
/// worktree.c:919 is never reached and neither the warning nor the hint is
/// printed. That unborn-worktree floor is not built here, so the caller reports
/// the resolver's own failure rather than inventing output stock never emits.
fn invalid_reference(
    repo: &gix::Repository,
    branch: &str,
    path: &str,
    new_branch: Option<&str>,
    quiet: bool,
    ac_lt_2: bool,
) -> Result<Option<ExitCode>> {
    // `can_use_local_refs()` (worktree.c:691-701) runs on every `ac < 2` arm —
    // through `dwim_orphan()` for the two DWIM branches and directly for
    // `--detach` — so its warning belongs to all of them and to none of the
    // `ac == 2` ones.
    if ac_lt_2 && !can_use_local_refs(repo, quiet)? {
        return Ok(None);
    }
    if !quiet && ac_lt_2 {
        // Both texts end in a newline, so `vadvise()` emits a blank `hint:` line
        // between the block and its `Disable this message with …` trailer.
        match new_branch {
            Some(name) => crate::advice::Advice::WorktreeAddOrphan.advise_in(
                repo,
                &format!(
                    "If you meant to create a worktree containing a new unborn branch\n\
                     (branch with no commits) for this repository, you can do so\n\
                     using the --orphan flag:\n\
                     \n\
                     \x20   git worktree add --orphan -b {name} {path}\n"
                ),
            ),
            None => crate::advice::Advice::WorktreeAddOrphan.advise_in(
                repo,
                &format!(
                    "If you meant to create a worktree containing a new unborn branch\n\
                     (branch with no commits) for this repository, you can do so\n\
                     using the --orphan flag:\n\
                     \n\
                     \x20   git worktree add --orphan {path}\n"
                ),
            ),
        };
    }
    eprintln!("fatal: invalid reference: {branch}");
    Ok(Some(ExitCode::from(128)))
}

/// Port of `can_use_local_refs()` (worktree.c:691-701): whether the repository
/// has anything a worktree could be started from. `HEAD` resolving is enough;
/// otherwise any `refs/heads/*` that resolves counts, and git warns that `HEAD`
/// is pointing somewhere it should not be. Only when neither holds does
/// `dwim_orphan()` conclude that an unborn branch is the only possibility.
///
/// The warning's message ends in a newline of its own, so `warning()` prints it
/// followed by a blank line — which is why stock's output has one before the
/// hint block.
fn can_use_local_refs(repo: &gix::Repository, quiet: bool) -> Result<bool> {
    if repo.head_id().is_ok() {
        return Ok(true);
    }
    let any_branch = repo
        .references()?
        .prefixed("refs/heads/")?
        .filter_map(std::result::Result::ok)
        .any(|mut r| r.peel_to_id_in_place().is_ok());
    if any_branch {
        if !quiet {
            eprintln!("warning: HEAD points to an invalid (or orphaned) reference.\n");
        }
        return Ok(true);
    }
    Ok(false)
}

/// Which of `<commit-ish>`, `-b`/`-B` and the DWIM branch this add starts from.
/// `unique_tracking_name()` (remote.c): the one remote-tracking ref that
/// `refs/heads/<name>` would be fetched into, or `None` when no remote or more
/// than one remote offers it.
///
/// git runs each remote's *fetch refspecs* over `refs/heads/<name>`
/// (`check_tracking_name()` → `refspec_find_match()`) and keeps the destination
/// only if that ref exists, so the answer follows a rewritten refspec rather
/// than assuming `refs/remotes/<remote>/<name>`. The same src-side match is done
/// here, mirroring the dst-side one [`super::branch`] already uses: a `*` in the
/// source matches by prefix and suffix, and whatever it captured is substituted
/// into the destination's `*`.
///
/// Ambiguity is a decline, not an error — two remotes carrying the branch leaves
/// `worktree add` starting from `HEAD`, which is what git does with the `NULL`
/// this returns.
fn unique_tracking_name(repo: &gix::Repository, name: &str) -> Option<String> {
    let src_ref = format!("refs/heads/{name}");
    let mut found: Option<String> = None;
    for remote_name in repo.remote_names() {
        let Ok(remote) = repo.find_remote(&*remote_name) else { continue };
        for spec in remote.refspecs(gix::remote::Direction::Fetch) {
            let gix::refspec::Instruction::Fetch(gix::refspec::instruction::Fetch::AndUpdate {
                src,
                dst,
                ..
            }) = spec.to_ref().instruction()
            else {
                continue;
            };
            let (src, dst) = (src.to_str_lossy().into_owned(), dst.to_str_lossy().into_owned());
            let candidate = match src.split_once('*') {
                Some((prefix, suffix)) => {
                    let matched = src_ref
                        .strip_prefix(prefix)
                        .and_then(|rest| rest.strip_suffix(suffix))
                        .filter(|_| src_ref.len() >= prefix.len() + suffix.len())?;
                    match dst.split_once('*') {
                        Some((dp, ds)) => format!("{dp}{matched}{ds}"),
                        None => dst.clone(),
                    }
                }
                None if src == src_ref => dst.clone(),
                None => continue,
            };
            if repo.try_find_reference(candidate.as_str()).ok().flatten().is_none() {
                continue;
            }
            match &found {
                // A second remote offering the same branch is ambiguous, and
                // git's DWIM gives up rather than choosing.
                Some(existing) if *existing != candidate => return None,
                Some(_) => {}
                None => found = Some(candidate),
            }
        }
    }
    found
}

fn resolve_start(
    repo: &gix::Repository,
    new_branch: Option<&str>,
    force_branch: bool,
    detach: bool,
    commit_ish: Option<&str>,
    dwim_name: &str,
    guessed_start: Option<&str>,
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
            // `--detach` asked otherwise. `check_branch_ref()` builds
            // `refs/heads/<name>` and asks whether *that* ref exists, so an
            // ambiguous name whose tag wins the rev-parse rules is still a branch
            // here — `git worktree add <path> dup` checks `refs/heads/dup` out
            // rather than detaching at `refs/tags/dup`.
            let branch = (!detach)
                .then(|| repo.try_find_reference(format!("refs/heads/{spec}").as_str()).ok())
                .flatten()
                .flatten()
                .map(|r| r.name().to_owned());
            Ok(match branch {
                Some(name) => Start::Branch(name, oid),
                None => Start::Detached(oid),
            })
        }
        // No commit-ish: `-b $(basename <path>)` off HEAD, or a detached HEAD.
        None => {
            // `dwim_branch()`'s remote guess, when `worktree.guessRemote` found
            // exactly one remote-tracking branch of that name: the *start point*
            // becomes that ref, so the new branch begins at the remote's tip and
            // the `git branch` below records it as the upstream.
            let from = guessed_start.unwrap_or("HEAD");
            let oid = peel(from)?;
            if detach {
                Ok(Start::Detached(oid))
            } else {
                Ok(Start::NewBranch {
                    name: FullName::try_from(format!("refs/heads/{dwim_name}"))?,
                    oid,
                    force: false,
                    from: from.to_string(),
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

/// `die_if_checked_out(branch, ignore_current_worktree = 1)` (branch.c:394): the
/// *other* worktree whose `HEAD` is on `branch`, if any.
///
/// ```c
/// wt = find_shared_symref(worktrees, "HEAD", branch);
/// if (wt && (!ignore_current_worktree || !wt->is_current)) {
///         skip_prefix(branch, "refs/heads/", &branch);
///         die(_("'%s' is already used by worktree at '%s'"), branch, wt->path);
/// }
/// ```
///
/// This is the check `checkout`/`switch` make before moving `HEAD` onto a branch
/// and before `-B`/`-C` resets one: a branch belongs to one worktree at a time,
/// and moving it from another would pull the ref out from under a checked-out
/// tree. `wt->is_current` is the worktree this command is running in, which is
/// why `git checkout -B <current-branch>` is not a refusal.
pub(super) fn used_by_other_worktree(repo: &gix::Repository, branch: &str) -> Option<PathBuf> {
    let full = format!("refs/heads/{branch}");
    for wt in collect(repo, u64::MAX).ok()? {
        // `die_if_checked_out(branch, ignore_current_worktree = 1)` (branch.c:847-862):
        // ```c
        //         if (worktrees[i]->is_current && ignore_current_worktree)
        //                 continue;
        // ```
        // — `wt->is_current`, not "the checkout is where I am". See [`Wt::is_current`].
        if wt.is_current {
            continue;
        }
        // `is_shared_symref()` (worktree.c:494-518) never answers for a bare one.
        if wt.is_bare {
            continue;
        }
        let HeadInfo::Branch { name, .. } = &wt.head else {
            continue;
        };
        if name.as_bstr() != full.as_bytes() {
            continue;
        }
        return Some(wt.path);
    }
    None
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
    // `create_branch()` (branch.c:615-631): `forcing` is set by the validation that finds the
    // branch already there, so `-B` over an existing branch logs `Reset to`, and `-B` that creates
    // one logs `Created from` like `-b` does.
    let forcing = force && repo.try_find_reference(name.as_ref()).ok().flatten().is_some();
    let message = match forcing {
        true => format!("branch: Reset to {from}"),
        false => format!("branch: Created from {from}"),
    };
    let edit = RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.into(),
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

/// The `logs/HEAD` lines `add_worktree()` leaves behind: the creation of the new `HEAD`, and —
/// when `reset_line` — the `reset --hard` that checks the worktree out.
fn write_worktree_reflog(repo: &gix::Repository, admin: &Path, oid: ObjectId, reset_line: bool) -> Result<()> {
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
    // `log_ref_write_fd()` (refs/files-backend.c) adds the tab *with* the message and nothing at
    // all without one, so the first line — `add_worktree()` creates `HEAD` with no message — ends
    // right after the committer.
    let mut text = format!("{} {} {sig}\n", zero.to_hex(), oid.to_hex());
    if reset_line {
        text.push_str(&format!(
            "{} {} {sig}\treset: moving to HEAD\n",
            oid.to_hex(),
            oid.to_hex()
        ));
    }
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
/// Port of `copy_sparse_checkout()` (builtin/worktree.c:345-359).
///
/// ```c
///         char *from_file = repo_git_path(the_repository, "info/sparse-checkout");
///         char *to_file = xstrfmt("%s/info/sparse-checkout", worktree_git_dir);
///         if (file_exists(from_file)) {
///                 if (safe_create_leading_directories(the_repository, to_file) ||
///                         copy_file(to_file, from_file, 0666))
///                         error(_("failed to copy '%s' to '%s'; sparse-checkout may not work correctly"), …);
///         }
/// ```
///
/// `repo_git_path` is the *current* worktree's git directory, not the common one,
/// because `info/sparse-checkout` is per-worktree (path.c:103). The failure is an
/// `error:` line and nothing more — `add_worktree()` never looks at the result, so
/// a worktree whose patterns could not be copied is still created.
fn copy_sparse_checkout(repo: &gix::Repository, admin: &Path) {
    let from = repo.git_dir().join("info").join("sparse-checkout");
    if !from.exists() {
        return;
    }
    let to = admin.join("info").join("sparse-checkout");
    let copied = std::fs::create_dir_all(admin.join("info")).and_then(|()| std::fs::copy(&from, &to));
    if copied.is_err() {
        eprintln!(
            "error: failed to copy '{}' to '{}'; sparse-checkout may not work correctly",
            path_to_string(&from),
            path_to_string(&to)
        );
    }
}

/// Port of `copy_filtered_worktree_config()` (builtin/worktree.c:361-397).
///
/// The copy is verbatim except for two keys the new worktree must not inherit:
/// a `core.bare = true` is unset (`repo_config_set_multivar_in_file_gently` with a
/// NULL value and the pattern `true`), and `core.worktree` is unset outright —
/// both would point the new checkout at the old one's tree.
fn copy_filtered_worktree_config(repo: &gix::Repository, admin: &Path) {
    let from = repo.git_dir().join("config.worktree");
    if !from.exists() {
        return;
    }
    let to = admin.join("config.worktree");
    if std::fs::create_dir_all(admin).and_then(|()| std::fs::copy(&from, &to)).is_err() {
        eprintln!(
            "error: failed to copy worktree config from '{}' to '{}'",
            path_to_string(&from),
            path_to_string(&to)
        );
        return;
    }
    let Ok(mut file) = gix::config::File::from_path_no_includes(to.clone(), gix::config::Source::Local)
    else {
        return;
    };
    let bare_is_true = file
        .raw_value_by("core", None, "bare")
        .ok()
        .is_some_and(|v| v.to_str_lossy().eq_ignore_ascii_case("true"));
    let has_worktree = file.raw_value_by("core", None, "worktree").is_ok();
    let mut changed = false;
    if bare_is_true || has_worktree {
        if let Ok(mut section) = file.section_mut("core", None) {
            if bare_is_true {
                while section.remove("bare").is_some() {
                    changed = true;
                }
            }
            if has_worktree {
                while section.remove("worktree").is_some() {
                    changed = true;
                }
            }
        }
    }
    if changed && std::fs::write(&to, file.to_bstring()).is_err() {
        eprintln!("error: failed to unset '{}' in '{}'", "core.bare", path_to_string(&to));
    }
}

fn checkout_into(
    repo: &gix::Repository,
    admin: &Path,
    path: &Path,
    oid: ObjectId,
    sparsity: Option<&super::sparse_checkout::Sparsity>,
) -> Result<()> {
    let tree = repo.find_commit(oid)?.tree_id()?.detach();
    let mut index = repo.index_from_tree(&tree)?;
    // The checkout child runs with `GIT_DIR` pointing at the administrative
    // directory the copy above just populated, so `unpack_trees()` runs with
    // `o->internal.cfg.apply_sparse_checkout` set and `apply_sparse_checkout()`
    // (unpack-trees.c:523-568) marks every excluded path `SKIP_WORKTREE` rather
    // than writing it out. `EXTENDED` is what carries the bit through
    // serialization, and forces index version 3 — the same widening git's own
    // index gets.
    if let Some(sparsity) = sparsity {
        let paths: Vec<gix::bstr::BString> = {
            let backing = index.path_backing();
            index.entries().iter().map(|e| e.path_in(backing).to_owned()).collect()
        };
        for (entry, entry_path) in index.entries_mut().iter_mut().zip(paths.iter()) {
            if !sparsity.includes(&entry_path.to_str_lossy()) {
                entry.flags.insert(
                    gix::index::entry::Flags::SKIP_WORKTREE | gix::index::entry::Flags::EXTENDED,
                );
            }
        }
    }
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
    // `add_worktree()` checks out through `unpack_trees()`, which ends in
    // `cache_tree_update(..., WRITE_TREE_SILENT | WRITE_TREE_REPAIR)`; the index it writes
    // therefore carries a fully valid `TREE` extension. An index built straight from a tree is
    // exactly what `prime_cache_tree()` (cache-tree.c:897) describes, so prime it from the same
    // tree rather than leaving the extension out — a later `write-tree` or `status` would
    // otherwise have to rebuild what git had already recorded.
    index.prime_cache_tree(&repo.objects, &tree)?;
    index.write_to(
        std::fs::File::create(admin.join("index"))?,
        crate::config::index_write_options(repo),
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

    let repo = crate::setup::discover()?;
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
    let worktrees_dir = repo.common_dir().join("worktrees");
    let admin = worktrees_dir.join(&id);
    if let Err(e) = std::fs::remove_dir_all(&admin) {
        return die(&format!(
            "failed to delete '{}': {}",
            path_to_string(&admin),
            errno_str(&e)
        ));
    }
    // `delete_worktrees_dir_if_empty()` (builtin/worktree.c:164, called at :1427): removing the
    // last linked worktree takes `worktrees/` with it, so the repository is left as it was before
    // any worktree existed. `rmdir` fails harmlessly while other worktrees remain.
    let _ = std::fs::remove_dir(&worktrees_dir);
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

    let repo = crate::setup::discover()?;
    let worktrees = collect(&repo, u64::MAX)?;
    let Some(wt) = find_worktree(&worktrees, arg) else {
        return die(&format!("'{arg}' is not a working tree"));
    };
    let Some(id) = wt.id.clone() else {
        return die(&format!("'{arg}' is a main working tree"));
    };
    // `git worktree move a b` where `b` is a directory moves the checkout *into* it,
    // keeping its own name — the same rule `mv` follows.
    let mut dest = PathBuf::from(dest_arg);
    if dest.is_dir() {
        let Some(name) = wt.path.file_name() else {
            return die(&format!(
                "could not figure out destination name from '{}'",
                path_to_string(&wt.path)
            ));
        };
        dest = dest.join(name);
    }
    // `check_candidate_path()` (builtin/worktree.c:317), which runs *before* the lock check and
    // names the destination it computed: `worktree move wt .` refuses `'./wt' already exists`,
    // not the `.` the caller typed. An empty directory there is not in the way.
    let occupied = match std::fs::read_dir(&dest) {
        Ok(mut entries) => entries.next().is_some(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => dest.exists(),
    };
    if occupied {
        return die(&format!("'{}' already exists", path_to_string(&dest)));
    }

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
    // ```c
    // if (validate_worktree(wt, &errmsg, 0))
    //         die(_("validation failed, cannot move working tree: %s"), errmsg.buf);
    // ```
    //
    // (`move_worktree()`, builtin/worktree.c.) `validate_worktree()` checks that
    // the checkout is still where the administrative directory says it is, and
    // its own message names the `.git` link that is missing. Without it a
    // worktree whose directory has been deleted reported the failed `rename(2)`
    // instead — a different message, and one that leaks the destination.
    let dotgit = wt.path.join(".git");
    if std::fs::symlink_metadata(&dotgit).is_err() {
        return die(&format!(
            "validation failed, cannot move working tree: '{}' does not exist",
            path_to_string(&dotgit)
        ));
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
