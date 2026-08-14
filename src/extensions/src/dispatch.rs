//! Subcommand routing for the shadow `git` binary.
//!
//! Two namespaces share one dispatch table:
//!   * **superset** verbs (`z*`) — the novel coordination layer, [`superset`].
//!   * **git-compat** porcelain — stock git subcommands served via gitoxide,
//!     ported incrementally, [`porcelain`].

use crate::{porcelain, superset};
use anyhow::Result;
use std::process::ExitCode;

/// zvcs-native extension verbs — the superset that stock git does not have.
pub const SUPERSET_VERBS: &[&str] = &[
    "zsync", "zbump", "zdaemon", "zconfig", "zrepos", "zreindex", "zjobs", "zjob", "zcommit", "zpush",
    "zsubmit", "zevents", "ztail", "zcommands", "zintercept", "zaudit", "zscan", "zsigs", "zreview", "zremote",
    "zrollback", "zsched",
    "zpin", "zunpin", "zbroadcast", "zhandoff", "zon", "zsince", "zcontend", "zwaitfor", "zgraph", "zrewind",
    "zguard", "zpolicy",
    "zrepl", "zbanner", "zclaim", "zunclaim", "zwho", "zstatus", "zlog", "zundo", "zsnapshot", "zrestore",
    "zsnapshots", "zworktree", "zstash", "zunstash", "zstashes", "zup", "zforeach", "zhook",
    "ztrigger", "zwatch", "zshadow", "zdashed", "zverbs", "zselectors", "zcd", "zpwd", "zls", "zenv", "zunset", "zecho",
    "zdoctor", "zmkdir", "ztouch", "zrm", "zcp", "zmv", "zcat", "zln",
    "zheads", "zdirty", "zbranches", "ztags", "zremotes", "zsize", "zage", "zpull", "zattach",
    "zgrep", "zahead", "zbehind", "zunpushed", "zunpulled", "zauthors", "zhot", "zconflicts",
    "zfetch", "zgc", "zfsck", "zprune", "zreset", "zabort", "zcheckout", "ztagall", "zcommitall", "zpushall", "zclean",
    "zwait", "zqueue", "zbarrier",
    "zstale", "zlast", "zbig", "zfiles", "zcommits", "zpristine", "zdivergent", "zorphans", "zsessions", "zidle", "zdashboard", "ztop",
    "zppid", "zprocs", "zprecache",
];

/// Every git-compat porcelain verb this dispatch table serves, generated from
/// the porcelain module set alongside the match arms below (scripts/wire_dispatch.pl)
/// so the two can never drift. Consumed by [`is_verb`].
pub const PORCELAIN_VERBS: &[&str] = &[
    // ---- BEGIN generated porcelain verbs (scripts/wire_dispatch.pl) ----
    "add",
    "am",
    "annotate",
    "apply",
    "archimport",
    "archive",
    "backfill",
    "bisect",
    "blame",
    "branch",
    "bugreport",
    "bundle",
    "cat-file",
    "check-attr",
    "check-ignore",
    "check-mailmap",
    "check-ref-format",
    "checkout",
    "checkout--worker",
    "checkout-index",
    "cherry",
    "cherry-pick",
    "clean",
    "clone",
    "column",
    "commit",
    "commit-graph",
    "commit-tree",
    "config",
    "count-objects",
    "credential",
    "credential-cache",
    "credential-cache--daemon",
    "credential-netrc",
    "credential-osxkeychain",
    "credential-store",
    "cvsexportcommit",
    "cvsimport",
    "cvsserver",
    "daemon",
    "describe",
    "diagnose",
    "diff",
    "diff-files",
    "diff-index",
    "diff-pairs",
    "diff-tree",
    "difftool",
    "difftool--helper",
    "fast-export",
    "fast-import",
    "fetch",
    "fetch-pack",
    "filter-branch",
    "fmt-merge-msg",
    "for-each-ref",
    "for-each-repo",
    "format-patch",
    "format-rev",
    "fsck",
    "fsck-objects",
    "fsmonitor--daemon",
    "gc",
    "get-tar-commit-id",
    "grep",
    "hash-object",
    "help",
    "history",
    "hook",
    "http-backend",
    "http-fetch",
    "http-push",
    "imap-send",
    "index-pack",
    "init",
    "init-db",
    "instaweb",
    "interpret-trailers",
    "jump",
    "last-modified",
    "log",
    "ls-files",
    "ls-remote",
    "ls-tree",
    "mailinfo",
    "mailsplit",
    "maintenance",
    "merge",
    "merge-base",
    "merge-file",
    "merge-index",
    "merge-octopus",
    "merge-one-file",
    "merge-ours",
    "merge-recursive",
    "merge-recursive-ours",
    "merge-recursive-theirs",
    "merge-resolve",
    "merge-subtree",
    "merge-tree",
    "mergetool",
    "mktag",
    "mktree",
    "multi-pack-index",
    "mv",
    "name-rev",
    "notes",
    "p4",
    "pack-objects",
    "pack-redundant",
    "pack-refs",
    "patch-id",
    "pickaxe",
    "prune",
    "prune-packed",
    "pull",
    "push",
    "quiltimport",
    "range-diff",
    "read-tree",
    "rebase",
    "receive-pack",
    "reflog",
    "refs",
    "remote",
    "remote-ext",
    "remote-fd",
    "remote-ftp",
    "remote-ftps",
    "remote-http",
    "remote-https",
    "repack",
    "replace",
    "replay",
    "repo",
    "request-pull",
    "rerere",
    "reset",
    "restore",
    "rev-list",
    "rev-parse",
    "revert",
    "rm",
    "send-email",
    "send-pack",
    "sh-i18n--envsubst",
    "shell",
    "shortlog",
    "show",
    "show-branch",
    "show-index",
    "show-ref",
    "sparse-checkout",
    "stage",
    "stash",
    "status",
    "stripspace",
    "submodule",
    "submodule--helper",
    "subtree",
    "switch",
    "symbolic-ref",
    "tag",
    "unpack-file",
    "unpack-objects",
    "update-index",
    "update-ref",
    "update-server-info",
    "upload-archive",
    "upload-archive--writer",
    "upload-pack",
    "url-parse",
    "var",
    "verify-commit",
    "verify-pack",
    "verify-tag",
    "version",
    "web--browse",
    "whatchanged",
    "worktree",
    "write-tree",
    // ---- END generated porcelain verbs ----
];

/// git's `NEED_WORK_TREE` commands, verbatim from the command table in `git.c`
/// (v2.55.0, lines 530-663): the ones `run_builtin()` puts through
/// `setup_work_tree()` before the builtin is entered, so they refuse in a bare
/// repository no matter what they were asked to do.
///
/// The flag is the whole rule for these; a command not listed here may still need
/// a work tree for *some* of its options and asks for it itself — `ls-files` for
/// its five worktree selectors (`builtin/ls-files.c:707`), `reset` for every mode
/// but `--soft` (`builtin/reset.c:471`), `update-index`, `rm`, `diff`,
/// `diff-index`, `read-tree -u`, `sparse-checkout`, `check-attr`, `grep` and
/// `describe --dirty` likewise.
const NEED_WORK_TREE: &[&str] = &[
    "add",
    "am",
    "check-ignore",
    "checkout",
    "checkout--worker",
    "checkout-index",
    "cherry-pick",
    "clean",
    "commit",
    "diff-files",
    "merge",
    "merge-recursive",
    "merge-recursive-ours",
    "merge-recursive-theirs",
    "merge-subtree",
    "mv",
    "pull",
    "rebase",
    "restore",
    "revert",
    "stage",
    "stash",
    "status",
    "switch",
];

/// Whether `sub` names a verb [`run`] dispatches — a superset verb or a ported
/// porcelain command. Alias expansion uses this to stop at a real command
/// (builtins win over an `alias.<cmd>` of the same name, exactly as git does)
/// rather than mistaking it for an alias.
pub fn is_verb(sub: &str) -> bool {
    SUPERSET_VERBS.contains(&sub) || PORCELAIN_VERBS.contains(&sub)
}

/// One-line usage for each z-verb, printed on `-h`/`--help`. `None` for anything
/// that is not a z-verb, so the dispatcher leaves other commands' `-h` alone.
fn z_usage(sub: &str) -> Option<&'static str> {
    Some(match sub {
        "zsync" => "usage: git zsync [--force] — reconcile submodules to origin/main AND fan this checkout's HEAD out to all its local dups (offline); --force hard-resets every dup (diverged/dirty included)",
        "zbump" => "usage: git zbump [<submodule-path>...] — forward-only submodule gitlink bumps",
        "zevents" => "usage: git zevents [-n <count>] [--kind commit|stage|status|reconcile] [--repo <substr>] [--json] [--no-follow] — one live feed of commits/reconciles/status-changes across the whole tree",
        "zpin" => "usage: git zpin [<path>...|list] — freeze repos from daemon autonomy (no autobump/reconcile until unpinned)",
        "zunpin" => "usage: git zunpin [<path>...|--all] — unfreeze pinned repos",
        "zbroadcast" => "usage: git zbroadcast [--to <session>] [<msg>...] — post an inter-agent message, or (no args) read your unread inbox",
        "zhandoff" => "usage: git zhandoff <repo-path> <session> — reassign a repo's claim to another agent session and notify it",
        "zon" => "usage: git zon [--kind commit|stage|status|reconcile] [--repo <substr>] -- <cmd> | git zon list | git zon rm <id> — run a command on a semantic feed event",
        "zsince" => "usage: git zsince <duration|snapshot> [--kind K] [--repo R] — everything across the tree since a time (90s/45m/2d/1h30m) or snapshot",
        "zcontend" => "usage: git zcontend — live agent-vs-agent contention: claims, per-repo job backlog, contested repos",
        "zwaitfor" => "usage: git zwaitfor <clean|idle|synced|<repo> <sha>> [--timeout <secs>] — block until a tree-wide state holds",
        "zgraph" => "usage: git zgraph — fleet topology: dup groups (same origin, multiple local checkouts)",
        "zrewind" => "usage: git zrewind <duration> [--dry-run] — restore the whole tree (repo + submodules) to the state it had <duration> ago via per-repo reflog reset",
        "zguard" => "usage: git zguard deny|warn <pattern> [--when detached|dirty|protected|unsigned] [-m <msg>] | list | rm <id> | clear | test <cmd>... — fleet-wide command policy that refuses/warns on matching git commands",
        // The alias needs its own line: `git zverbs` prints one usage per verb,
        // and a shared string would list `zguard` twice and `zpolicy` never.
        "zpolicy" => "usage: git zpolicy <deny|warn|list|rm|clear|test> ... — alias of `git zguard`",
        "ztail" => "usage: git ztail [-n <count>] [--kind commit|stage|status|reconcile] [--repo <substr>] [--json] [--no-follow] — alias of git zevents",
        "zcommands" => "usage: git zcommands [-n <count>] [--repo <substr>] [--json] [--no-follow] [--off] [--clear] — live feed of every git command run across the fleet",
        "zintercept" => "usage: git zintercept before|after|around <pattern> -- <cmd> | list | remove <id> | clear — AOP hooks that run advice around matching git commands",
        "zaudit" => "usage: git zaudit [--agent <ppid>] [--repo <substr>] [--cmd <substr>] [--mutating] [--summary] [-n <count>] [--json] — queryable audit trail over the fleet command log",
        "zscan" => "usage: git zscan [selectors] — parallel secret scan of tracked content across indexed repos (exits non-zero if any found)",
        "zsigs" => "usage: git zsigs [selectors] [-n <count>] — fleet commit-signature check: flag unsigned/bad/unverifiable HEAD commits (gpg/ssh via %G?); exits non-zero if any found",
        "zreview" => "usage: git zreview [selectors] — aggregate the pending uncommitted change (short status + diffstat) of every dirty indexed repo",
        "zremote" => "usage: git zremote set <old> <new> [selectors] [-n|--dry-run] — rewrite remote URLs across the fleet, replacing substring <old> with <new>",
        "zrollback" => "usage: git zrollback [selectors] [--steps <n>] [--apply] [--force] — fleet-wide undo of the last mutating op (reset --hard HEAD@{n}); dry-run unless --apply; skips dirty/mid-op/would-diverge repos unless --force",
        "zsched" => "usage: git zsched add <duration> -- <cmd> | list | rm <id> | clear | run <id> — daemon-hosted scheduled fleet commands (a built-in cron for the tree)",
        "zdaemon" => "usage: git zdaemon <start|stop|restart|status|info|ping|log>",
        "zconfig" => "usage: git zconfig [<name> [on|off|<count>|default]] — toggle daemon features (see `git help zconfig`)",
        "zrepos" => "usage: git zrepos [<pattern>...] — list indexed repos; patterns filter by case-insensitive substring",
        "zreindex" => "usage: git zreindex [--sync|--async] [<path>...] — (re)crawl for git repos and refresh the index",
        "zshadow" => "usage: git zshadow [<dir>] [-n|--print] [--all] — install the ~/.zvcs shadow (git shim, dashed links, man pages, zsh _git) and print the PATH/MANPATH/fpath lines to eval",
        "zdashed" => "usage: git zdashed [<dir>] — install a git-<verb> symlink for every builtin",
        "zjobs" => "usage: git zjobs [-n <count>] — list recent ledger jobs (newest first)",
        "zjob" => "usage: git zjob <id> | git zjob <stop|restart> <id> — show or control a job",
        "zcommit" => "usage: git zcommit [<path>...] -m <msg> [--push] — queue an atomic staged-commit job",
        "zpush" => "usage: git zpush [<refspec>] — queue an async push job with a network-free ff pre-flight",
        "zsubmit" => "usage: git zsubmit [--] <command> [args...] — run an arbitrary command as an async daemon job (track with zjobs/zjob)",
        "zrepl" => "usage: git zrepl — interactive console over every zvcs command (z-verbs + git porcelain)",
        "zbanner" => "usage: git zbanner [--color|--no-color] — reprint the zrepl startup banner (logo + live system/verb/repo stats)",
        "zclaim" => "usage: git zclaim [<path>] — lease a repo for this session",
        "zunclaim" => "usage: git zunclaim [--force] [<path>] — release a lease on a repo",
        "zwho" => "usage: git zwho — list active claims (who is working what)",
        "zstatus" => "usage: git zstatus [--all] — cached working-tree status of indexed repos",
        "zprecache" => "usage: git zprecache [-n <commits>] [-q] — precompute the log caches (abbreviations, --stat tallies) for recent commits",
        "zlog" => "usage: git zlog [-n <count>] — machine-wide reflog timeline across all indexed repos",
        "zundo" => "usage: git zundo [<path>] — rewind a repo one reflog step (reset --hard to previous HEAD)",
        "zsnapshot" => "usage: git zsnapshot <name> — record the tree's HEADs as a restore point",
        "zrestore" => "usage: git zrestore <name> — reset the whole tree back to a snapshot",
        "zsnapshots" => "usage: git zsnapshots — list snapshot names and their repo counts",
        "zworktree" => "usage: git zworktree <add <name>|list|remove <name>> — tree-wide private worktrees",
        "zstash" => "usage: git zstash [<name>] — stash every dirty repo in the tree under <name>",
        "zunstash" => "usage: git zunstash [<name>] — pop the tree-wide stash back (LIFO)",
        "zstashes" => "usage: git zstashes — list tree-wide stashes and their repo counts",
        "zup" => "usage: git zup [<path>] — reconcile the tree at cwd (or <path>) to latest",
        "zforeach" => "usage: git zforeach [<pattern>...|--repo <p>|--dirty|--ahead|--behind|--claimed|--session <s>] -- <command>...",
        "zhook" => "usage: git zhook <set <command>|unset|show|list|test>",
        "ztrigger" => "usage: git ztrigger <DIR> <command>... [--throttle <dur>] | git ztrigger <list|rm DIR|test DIR|tail|top> — run a command on any file change in DIR (leading-edge throttle, default 500ms; tail/top show fires live)",
        "zwatch" => "usage: git zwatch <DIR> | git zwatch <list|rm DIR> — watch DIR (index + cached status) without a command",
        "zverbs" => "usage: git zverbs [--json|--html] — list every zvcs extension verb and its usage (--json for scripting, --html emits the full docs/reference.html reference page)",
        "zselectors" => "usage: git zselectors — print the shared [selectors] grammar (see also `git help zselectors`)",
        "zcd" => "usage: git zcd [<dir>|-] — change the working directory (for the zrepl console)",
        "zpwd" => "usage: git zpwd — print the working directory",
        "zls" => "usage: git zls [-alrt] [<path>] — git-aware directory listing (per-file status like eza --git)",
        "zenv" => "usage: git zenv [<NAME=VALUE>...|<NAME>...] — print, set, or query environment variables",
        "zunset" => "usage: git zunset <NAME>... — remove environment variables",
        "zecho" => "usage: git zecho [-n] [<arg>...] — print arguments joined by a space",
        "zdoctor" => "usage: git zdoctor — health check of the zvcs environment (shadow, daemon, ledger, man pages)",
        "zmkdir" => "usage: git zmkdir [-p] <dir>... — create directories",
        "ztouch" => "usage: git ztouch <file>... — create files or bump their mtime",
        "zrm" => "usage: git zrm [-r] [-f] <path>... — remove files/directories (filesystem, not git rm)",
        "zcp" => "usage: git zcp [-r] <src>... <dst> — copy files/directories",
        "zmv" => "usage: git zmv <src>... <dst> — move/rename files/directories",
        "zcat" => "usage: git zcat <file>... — print file contents to stdout",
        "zln" => "usage: git zln [-s] <target> <link> — create a hard link or symlink (-s)",
        "zheads" => "usage: git zheads [selectors] — HEAD branch/id (+dirty) of each indexed repo, in parallel",
        "zdirty" => "usage: git zdirty [selectors] — list indexed repos with uncommitted tracked changes, in parallel",
        "zbranches" => "usage: git zbranches [selectors] — local branches of each indexed repo, in parallel",
        "ztags" => "usage: git ztags [selectors] — tag count of each indexed repo, in parallel",
        "zremotes" => "usage: git zremotes [selectors] — remotes and fetch URLs of each indexed repo",
        "zsize" => "usage: git zsize [selectors] — on-disk .git size of each indexed repo (largest first)",
        "zage" => "usage: git zage [selectors] — how long ago each indexed repo's HEAD commit was made",
        "zpull" => "usage: git zpull [selectors] — parallel fetch + fast-forward of every indexed repo (ff-only)",
        "zattach" => "usage: git zattach [selectors] — re-attach every detached-HEAD indexed repo to its mainline branch (local, no-clobber, no network)",
        "zgrep" => "usage: git zgrep [selectors] [-i] <pattern> — parallel regex search of tracked content across indexed repos",
        "zahead" => "usage: git zahead [selectors] — indexed repos with commits not yet on their upstream",
        "zbehind" => "usage: git zbehind [selectors] — indexed repos whose upstream is ahead of them",
        "zunpushed" => "usage: git zunpushed [selectors] — per-repo unpushed commits (id + summary); the detailed zahead",
        "zunpulled" => "usage: git zunpulled [selectors] — per-repo commits the upstream has that local lacks; the detailed zbehind",
        "zauthors" => "usage: git zauthors [selectors] — commit counts by author across indexed repos, ranked",
        "zhot" => "usage: git zhot [selectors] [<days>] — indexed repos ranked by commits in the last <days> (default 30)",
        "zconflicts" => "usage: git zconflicts [selectors] — indexed repos mid-merge/rebase/cherry-pick/revert/bisect or with conflicts",
        "zfetch" => "usage: git zfetch [selectors] — parallel git fetch across every indexed repo",
        "zgc" => "usage: git zgc [selectors] — parallel git gc across every indexed repo",
        "zfsck" => "usage: git zfsck [selectors] — parallel git fsck across every indexed repo",
        "zprune" => "usage: git zprune [selectors] — parallel git prune across every indexed repo",
        "zreset" => "usage: git zreset [selectors] [--soft|--mixed|--hard] [<ref>] — parallel git reset across indexed repos",
        "zabort" => "usage: git zabort [selectors] — abort an in-progress merge/rebase/cherry-pick/revert in every mid-op repo",
        "zcheckout" => "usage: git zcheckout [selectors] <branch> — check out <branch> in every indexed repo that has it",
        "ztagall" => "usage: git ztagall [selectors] <tag> — create <tag> at HEAD in every indexed repo",
        "zcommitall" => "usage: git zcommitall [selectors] -m <msg> — commit tracked changes (commit -a) in every dirty repo",
        "zpushall" => "usage: git zpushall [selectors] — push every indexed repo that is ahead of its upstream",
        "zclean" => "usage: git zclean -f [selectors] — remove untracked files (git clean -fd) in every indexed repo (-f required)",
        "zwait" => "usage: git zwait [<path>] — block until the repo's async jobs drain",
        "zqueue" => "usage: git zqueue — list queued/running async jobs",
        "zbarrier" => "usage: git zbarrier — block until the whole async job queue is idle",
        "zstale" => "usage: git zstale [selectors] [<days>] — indexed repos whose HEAD is older than <days> (default 90)",
        "zlast" => "usage: git zlast [selectors] — indexed repos ordered by HEAD commit time, newest first",
        "zbig" => "usage: git zbig [selectors] [<n>] — largest tracked files across indexed repos (top <n>, default 20)",
        "zfiles" => "usage: git zfiles [selectors] — tracked file count of each indexed repo (largest first)",
        "zcommits" => "usage: git zcommits [selectors] — HEAD-history commit count of each indexed repo (deepest first)",
        "zpristine" => "usage: git zpristine [selectors] — indexed repos that are clean, attached, and in sync (nothing to do)",
        "zdivergent" => "usage: git zdivergent [selectors] — indexed repos both ahead of and behind their upstream",
        "zorphans" => "usage: git zorphans [selectors] — indexed repos with no remote configured",
        "zsessions" => "usage: git zsessions — active sessions ranked by repos held",
        "zidle" => "usage: git zidle [selectors] — indexed repos with no active claim (free to pick up)",
        "zdashboard" => "usage: git zdashboard — instant one-screen health summary from the status cache + ledger",
        "zppid" => "usage: git zppid [--json] — per-process commit tally: each invoking process (ppid) and how many commits it has landed",
        "zprocs" => "usage: git zprocs [--json] — per-process command breakdown: how many of each mutating verb (commit/push/add/merge/…) each process has run",
        "ztop" => "usage: git ztop [selectors] [--interval <secs>] [--once] [--mono] — live htop-style fleet monitor, most churn on top",
        _ => return None,
    })
}

/// `git zverbs` — print every superset (`z*`) verb with its one-line usage. The
/// text is [`z_usage`] itself, minus the `usage: git ` lead-in, so the listing is
/// the same source of truth each verb's own `-h` prints and can never drift.
fn print_verbs(args: &[String]) -> Result<ExitCode> {
    if args.iter().any(|a| a == "--html") {
        print!("{}", superset::manpage::html_reference());
        return Ok(ExitCode::SUCCESS);
    }
    let json = args.iter().any(|a| a == "--json");
    for verb in SUPERSET_VERBS {
        if let Some(usage) = z_usage(verb) {
            let u = usage.strip_prefix("usage: git ").unwrap_or(usage);
            if json {
                println!("{}", serde_json::json!({"verb": verb, "usage": u}));
            } else {
                println!("{u}");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Index-mutating verbs whose primary operation takes the repo write lock. On
/// lane contention these auto-queue as a job instead of blocking (see [`run`]).
/// Deliberately excludes verbs that only lock conditionally or are not index
/// writers (`config` reads, `fetch`/`fetch-pack`/`http-fetch`, `remote`, `init`),
/// so those never get deferred.
const LOCK_VERBS: &[&str] = &[
    "add", "am", "apply", "checkout", "cherry-pick", "commit", "commit-tree", "merge",
    "mv", "rebase", "reset", "restore", "revert", "rm", "stage", "stash", "switch",
    "read-tree", "update-index", "write-tree", "tag", "branch", "notes", "replace",
    "replay", "rerere", "sparse-checkout", "submodule", "mktag", "mktree",
    // Superset verbs that take the repo's write lane. `zbump` (forward-only
    // submodule gitlink bumps) is THE pointer-bump path — `git add` deliberately
    // skips gitlinks. `zsync` fast-forwards local dups off the source repo's lane.
    // Both must queue under contention like `commit` rather than block.
    "zbump", "zsync",
];

/// Whether deferring this command to the job queue could silently swallow work
/// somebody is synchronously waiting for.
///
/// Queueing is a fair-scheduling feature for a command a HUMAN (or an agent)
/// typed: `zvcs: queued job #N`, exit 0, the work lands shortly after. It is a
/// correctness bug for a command another `git` process spawned as a child and is
/// blocked on — hooks, `add -p`'s `apply`, `clone --recurse-submodules`'s
/// `submodule update`, `subtree`'s and `am`'s re-execs, `filter-branch`'s
/// filters. Those parents read exit 0 as "it worked".
///
/// Answers `true` when an ancestor is a `git`/`zvcs` process, and also when the
/// platform will not report our ancestry at all: blocking on a contended lane is
/// never a wrong answer, whereas queueing can be. `ZVCS_NO_QUEUE` forces `true`
/// for callers this cannot see (a child across an ssh or container boundary).
fn queueing_would_swallow_work() -> bool {
    if std::env::var_os("ZVCS_NO_QUEUE").is_some() {
        return true;
    }
    superset::zppid::git_ancestor().unwrap_or(true)
}

pub fn run(sub: &str, args: &[String]) -> Result<ExitCode> {
    // Fleet command log: record this invocation when `git zcommands` has turned
    // logging on. A single `stat` (no work) when it is off, so the hot path pays
    // essentially nothing; best-effort, never fails the command.
    superset::zcommands::log_invocation(sub, args);

    // Fleet-wide command policy (`git zguard`): refuse or warn on a matching
    // command before it runs. A single `stat` when no rule is set. The policy
    // management verbs are exempt so a bad rule can never lock you out of fixing it.
    if sub != "zguard" && sub != "zpolicy" && superset::guard::active() {
        match superset::guard::check(sub, args) {
            superset::guard::Verdict::Deny(msg) => {
                eprintln!("{msg}");
                return Ok(ExitCode::from(1));
            }
            superset::guard::Verdict::Warn(msgs) => {
                for m in &msgs {
                    eprintln!("{m}");
                }
            }
            superset::guard::Verdict::Allow => {}
        }
    }

    // AOP intercepts (ported from zshrs): if before/after/around advice is
    // registered for this command, orchestrate it. Returns Some when interception
    // handled the command (around, or before+after wrapping); None otherwise —
    // including the before-only case, whose advice has already run by then.
    if let Some(result) = superset::intercepts::maybe_intercept(sub, args) {
        return result;
    }

    // `handle_builtin()` (git.c): `git <cmd> --help` is rewritten to
    // `git help <cmd>`, which opens the manual page. Only the *first* argument
    // counts — `git log --oneline --help` stays with `log`, where parse-options
    // treats it as `-h`. Every command reached this way used to answer with its
    // own "unknown option" instead of documentation.
    if args.first().is_some_and(|a| a == "--help") && !sub.starts_with('z') {
        let mut help_args: Vec<String> = vec![sub.to_string()];
        help_args.extend(args[1..].iter().cloned());
        return porcelain::help(&help_args);
    }

    // Every z-verb answers `-h`/`--help` with a one-line usage, uniformly and
    // before dispatch. `z_usage` returns `None` for anything that is not a known
    // z-verb, so this never intercepts a porcelain command's own `-h`.
    if sub.starts_with('z') && args.iter().any(|a| a == "-h" || a == "--help") {
        if let Some(usage) = z_usage(sub) {
            println!("{usage}");
            return Ok(ExitCode::SUCCESS);
        }
    }

    // git.c:499-500, `run_builtin()`:
    //
    //     if (!help && p->option & NEED_WORK_TREE)
    //             setup_work_tree(the_repository);
    //
    // The gate is the dispatcher's, not the builtin's, which is why it precedes
    // everything a `NEED_WORK_TREE` command would otherwise say — its usage error,
    // its "nothing to commit", its "Already up to date." A lone `-h` (or
    // `--help-all`) skips it, since git.c:474-477 demotes that to a gentle setup so
    // `git <cmd> -h` answers outside a repository too.
    //
    // Only a repository that was found but has no work tree refuses here: when
    // there is none at all, git's `RUN_SETUP` has already died with "not a git
    // repository", which each command still reports for itself.
    let help_only = args.len() == 1 && (args[0] == "-h" || args[0] == "--help-all");
    if !help_only && NEED_WORK_TREE.contains(&sub) {
        if let Ok(repo) = gix::discover(".") {
            // `setup_work_tree()` dies the same way for a work tree that is not
            // configured and for one it cannot `chdir()` into (setup.c:503-505).
            if !repo.workdir().is_some_and(|wt| wt.is_dir()) {
                return Err(crate::fatal::need_work_tree());
            }
        }
    }

    // Lock-contention → queue. For an index-mutating verb, try to take the repo's
    // write lane WITHOUT blocking: if it is already held, submit this command as a
    // job (it will run on the daemon's fair FIFO) and return its number instead of
    // waiting. If the lane is free we hold it here for the whole command (the
    // command's own inner `acquire` is a reentrant no-op). `ZVCS_QUEUED` marks a
    // job's own re-run so it BLOCKS on the lock rather than re-queueing (loop guard);
    // no daemon → run inline exactly as before.
    // `-h`/`--help` never writes the index, so a contended repo must not turn a
    // help request into a queued job with empty output.
    let is_help = args.iter().any(|a| a == "-h" || a == "--help");
    // Interactive hunk selection (`add -p`, `reset -p`, `checkout -p`,
    // `restore -p`, `commit -p`/`-i`) writes nothing itself: it renders hunks,
    // waits on the user, and hands each accepted selection to a `git apply`
    // CHILD, which takes the lane for the microseconds it needs. Holding the
    // lane in the parent would (a) block every other zvcs writer for as long as
    // the user reads, and (b) deadlock the child, which would find the lane busy
    // and queue itself as a job instead of applying. Same reasoning as `-h`: the
    // verb is in `LOCK_VERBS` for its non-interactive form only.
    let is_interactive_patch = matches!(
        sub,
        "add" | "stage" | "reset" | "checkout" | "restore" | "commit" | "stash"
    ) && args.iter().any(|a| {
        a == "-p"
            || a == "--patch"
            // `-i`/`--interactive` only reaches the hunk selector from `add` and
            // `commit`; SHORT `-i` is `--include` for `commit` and unrelated for
            // `am`, so only `add`/`stage` may spell it that way. `commit
            // --interactive` was missing here while `commit.rs` already skipped its
            // own lock for it, so dispatch held the lane across the selector — the
            // exact position the `commit -p` fix was meant to vacate.
            || (matches!(sub, "add" | "stage") && (a == "-i" || a == "--interactive"))
            || (sub == "commit" && a == "--interactive")
    });
    let is_lock_verb = LOCK_VERBS.contains(&sub) && !is_help && !is_interactive_patch;
    let queued_rerun = std::env::var_os("ZVCS_QUEUED").is_some();
    let _queue_guard = if is_lock_verb && !queued_rerun {
        match gix::discover(".") {
            Ok(repo) => match crate::lock::RepoLock::try_acquire(repo.git_dir()) {
                crate::lock::TryLock::Held(g) => Some(g),
                crate::lock::TryLock::Busy { owner_resolved } if queueing_would_swallow_work() => {
                    // We are a synchronous child of another `git` process, which
                    // is blocked waiting for our EFFECT. Becoming a job here would
                    // print `queued job #N`, exit 0, and hand the parent a success
                    // it did not earn — the wrong-answer-with-success-status shape
                    // that swallowed `commit -p`'s hunks and `clone
                    // --recurse-submodules`'s submodules. Block on the lane instead
                    // and do the real work; `acquire` returns a no-op guard at once
                    // when the holder is one of our own ancestors, so the parent's
                    // own hold cannot deadlock us.
                    if !owner_resolved {
                        // …unless the coordinator cannot name the holder, in which
                        // case that ancestor check is unavailable and waiting could
                        // be waiting on our own parent, forever. Fail, loudly and
                        // non-zero: a caller that is blocked on us must not read
                        // this as success, and must not read it as a hang either.
                        eprintln!(
                            "zvcs: {sub}: repo lane is busy and the coordinator cannot name its holder \
                             (it predates the lane-owner query); refusing to defer a command another \
                             git process is waiting on. Restart it: git zdaemon restart"
                        );
                        return Ok(ExitCode::from(1));
                    }
                    Some(crate::lock::RepoLock::acquire(repo.git_dir()))
                }
                crate::lock::TryLock::Busy { .. } => {
                    return crate::superset::queue::queue_verb(sub, args)
                }
                crate::lock::TryLock::NoDaemon => None,
            },
            Err(_) => None,
        }
    } else {
        None
    };

    // The lane above only serializes zvcs writers. A FOREIGN writer (stock git,
    // an IDE, a hook) holds git's `O_EXCL` `.git/index.lock`, which the daemon
    // cannot see and the ported index writer refuses to wait on. Give that lock a
    // short budget to clear before running, so the usual millisecond overlap
    // becomes a wait rather than the error it used to be. A queued job's re-run
    // waits too — by then it owns the lane, and only a foreign holder is left.
    if is_lock_verb {
        if let Ok(repo) = gix::discover(".") {
            crate::lock::wait_for_foreign_index_lock(repo.git_dir());
        }
    }

    // Per-process commit tally: for a commit-producing verb, capture HEAD now so we
    // can credit the running process (by ppid) after the command if HEAD advanced.
    // `.then` skips the gix probe entirely for every non-commit verb, so the hot
    // path pays nothing.
    let commit_head_before = superset::zppid::is_commit_verb(sub).then(superset::zppid::head_commit);
    let track_mutating = superset::zppid::is_mutating_verb(sub);
    // Resolving WHO ran this command costs a few process-info syscalls; start
    // them now so they run alongside the command rather than after it.
    let attribution = track_mutating.then(superset::zppid::spawn_resolve);

    let result = match sub {
        // ---- superset (novel) ----
        "zsync" => superset::zsync(args),
        "zbump" => superset::zbump(args),
        "zevents" | "ztail" => superset::zevents(args),
        "zpin" => superset::zpin(args),
        "zunpin" => superset::zunpin(args),
        "zbroadcast" => superset::zbroadcast(args),
        "zhandoff" => superset::zhandoff(args),
        "zon" => superset::zon(args),
        "zsince" => superset::zsince(args),
        "zcontend" => superset::zcontend(args),
        "zwaitfor" => superset::zwaitfor(args),
        "zgraph" => superset::zgraph(args),
        "zrewind" => superset::zrewind(args),
        "zguard" | "zpolicy" => superset::guard::zguard(args),
        "zcommands" => superset::zcommands(args),
        "zintercept" => superset::zintercept(args),
        "zaudit" => superset::zaudit(args),
        "zscan" => superset::zscan(args),
        "zsigs" => superset::zsigs(args),
        "zreview" => superset::zreview(args),
        "zremote" => superset::zremote(args),
        "zrollback" => superset::zrollback(args),
        "zsched" => superset::zsched(args),
        "zdaemon" => superset::zdaemon(args),
        "zconfig" => superset::zconfig(args),
        "zrepos" => superset::zrepos(args),
        "zreindex" => superset::zreindex(args),
        "zshadow" => superset::zshadow(args),
        "zdashed" => superset::zdashed(args),
        "zjobs" => superset::zjobs(args),
        "zjob" => superset::zjob(args),
        "zcommit" => superset::zcommit(args),
        "zpush" => superset::zpush(args),
        "zsubmit" => superset::zsubmit(args),
        "zrepl" => superset::zrepl(args),
        "zbanner" => superset::zbanner(args),
        "zclaim" => superset::zclaim(args),
        "zunclaim" => superset::zunclaim(args),
        "zwho" => superset::zwho(args),
        "zstatus" => superset::zstatus(args),
        "zprecache" => superset::zprecache(args),
        "zlog" => superset::zlog(args),
        "zundo" => superset::zundo(args),
        "zsnapshot" => superset::zsnapshot(args),
        "zrestore" => superset::zrestore(args),
        "zsnapshots" => superset::zsnapshots(args),
        "zworktree" => superset::zworktree(args),
        "zstash" => superset::zstash(args),
        "zunstash" => superset::zunstash(args),
        "zstashes" => superset::zstashes(args),
        "zup" => superset::zup(args),
        "zforeach" => superset::zforeach(args),
        "zhook" => superset::zhook(args),
        "ztrigger" => superset::ztrigger(args),
        "zwatch" => superset::zwatch(args),
        "zverbs" => print_verbs(args),
        "zselectors" => superset::zselectors(args),
        "zcd" => superset::zcd(args),
        "zpwd" => superset::zpwd(args),
        "zls" => superset::zls(args),
        "zenv" => superset::zenv(args),
        "zunset" => superset::zunset(args),
        "zecho" => superset::zecho(args),
        "zdoctor" => superset::zdoctor(args),
        "zmkdir" => superset::zmkdir(args),
        "ztouch" => superset::ztouch(args),
        "zrm" => superset::zrm(args),
        "zcp" => superset::zcp(args),
        "zmv" => superset::zmv(args),
        "zcat" => superset::zcat(args),
        "zln" => superset::zln(args),
        "zheads" => superset::zheads(args),
        "zdirty" => superset::zdirty(args),
        "zbranches" => superset::zbranches(args),
        "ztags" => superset::ztags(args),
        "zremotes" => superset::zremotes(args),
        "zsize" => superset::zsize(args),
        "zage" => superset::zage(args),
        "zpull" => superset::zpull(args),
        "zattach" => superset::zattach(args),
        "zgrep" => superset::zgrep(args),
        "zahead" => superset::zahead(args),
        "zbehind" => superset::zbehind(args),
        "zunpushed" => superset::zunpushed(args),
        "zunpulled" => superset::zunpulled(args),
        "zauthors" => superset::zauthors(args),
        "zhot" => superset::zhot(args),
        "zconflicts" => superset::zconflicts(args),
        "zfetch" => superset::zfetch(args),
        "zgc" => superset::zgc(args),
        "zfsck" => superset::zfsck(args),
        "zprune" => superset::zprune(args),
        "zreset" => superset::zreset(args),
        "zabort" => superset::zabort(args),
        "zcheckout" => superset::zcheckout(args),
        "ztagall" => superset::ztagall(args),
        "zcommitall" => superset::zcommitall(args),
        "zpushall" => superset::zpushall(args),
        "zclean" => superset::zclean(args),
        "zwait" => superset::zwait(args),
        "zqueue" => superset::zqueue(args),
        "zbarrier" => superset::zbarrier(args),
        "zstale" => superset::zstale(args),
        "zlast" => superset::zlast(args),
        "zbig" => superset::zbig(args),
        "zfiles" => superset::zfiles(args),
        "zcommits" => superset::zcommits(args),
        "zpristine" => superset::zpristine(args),
        "zdivergent" => superset::zdivergent(args),
        "zorphans" => superset::zorphans(args),
        "zsessions" => superset::zsessions(args),
        "zidle" => superset::zidle(args),
        "zdashboard" => superset::zdashboard(args),
        "zppid" => superset::zppid(args),
        "zprocs" => superset::zprocs(args),
        "ztop" => superset::ztop(args),

        // ---- BEGIN generated porcelain arms (scripts/wire_dispatch.pl) ----
        "add" => porcelain::add(args),
        "am" => porcelain::am(args),
        "annotate" => porcelain::annotate(args),
        "apply" => porcelain::apply(args),
        "archimport" => porcelain::archimport(args),
        "archive" => porcelain::archive(args),
        "backfill" => porcelain::backfill(args),
        "bisect" => porcelain::bisect(args),
        "blame" => porcelain::blame(args),
        "branch" => porcelain::branch(args),
        "bugreport" => porcelain::bugreport(args),
        "bundle" => porcelain::bundle(args),
        "cat-file" => porcelain::cat_file(args),
        "check-attr" => porcelain::check_attr(args),
        "check-ignore" => porcelain::check_ignore(args),
        "check-mailmap" => porcelain::check_mailmap(args),
        "check-ref-format" => porcelain::check_ref_format(args),
        "checkout" => porcelain::checkout(args),
        "checkout--worker" => porcelain::checkout__worker(args),
        "checkout-index" => porcelain::checkout_index(args),
        "cherry" => porcelain::cherry(args),
        "cherry-pick" => porcelain::cherry_pick(args),
        "clean" => porcelain::clean(args),
        "clone" => porcelain::clone(args),
        "column" => porcelain::column(args),
        "commit" => porcelain::commit(args),
        "commit-graph" => porcelain::commit_graph(args),
        "commit-tree" => porcelain::commit_tree(args),
        "config" => porcelain::config(args),
        "count-objects" => porcelain::count_objects(args),
        "credential" => porcelain::credential(args),
        "credential-cache" => porcelain::credential_cache(args),
        "credential-cache--daemon" => porcelain::credential_cache__daemon(args),
        "credential-netrc" => porcelain::credential_netrc(args),
        "credential-osxkeychain" => porcelain::credential_osxkeychain(args),
        "credential-store" => porcelain::credential_store(args),
        "cvsexportcommit" => porcelain::cvsexportcommit(args),
        "cvsimport" => porcelain::cvsimport(args),
        "cvsserver" => porcelain::cvsserver(args),
        "daemon" => porcelain::daemon(args),
        "describe" => porcelain::describe(args),
        "diagnose" => porcelain::diagnose(args),
        "diff" => porcelain::diff(args),
        "diff-files" => porcelain::diff_files(args),
        "diff-index" => porcelain::diff_index(args),
        "diff-pairs" => porcelain::diff_pairs(args),
        "diff-tree" => porcelain::diff_tree(args),
        "difftool" => porcelain::difftool(args),
        "difftool--helper" => porcelain::difftool__helper(args),
        "fast-export" => porcelain::fast_export(args),
        "fast-import" => porcelain::fast_import(args),
        "fetch" => porcelain::fetch(args),
        "fetch-pack" => porcelain::fetch_pack(args),
        "filter-branch" => porcelain::filter_branch(args),
        "fmt-merge-msg" => porcelain::fmt_merge_msg(args),
        "for-each-ref" => porcelain::for_each_ref(args),
        "for-each-repo" => porcelain::for_each_repo(args),
        "format-patch" => porcelain::format_patch(args),
        "format-rev" => porcelain::format_rev(args),
        "fsck" => porcelain::fsck(args),
        "fsck-objects" => porcelain::fsck_objects(args),
        "fsmonitor--daemon" => porcelain::fsmonitor__daemon(args),
        "gc" => porcelain::gc(args),
        "get-tar-commit-id" => porcelain::get_tar_commit_id(args),
        "grep" => porcelain::grep(args),
        "hash-object" => porcelain::hash_object(args),
        "help" => porcelain::help(args),
        "history" => porcelain::history(args),
        "hook" => porcelain::hook(args),
        "http-backend" => porcelain::http_backend(args),
        "http-fetch" => porcelain::http_fetch(args),
        "http-push" => porcelain::http_push(args),
        "imap-send" => porcelain::imap_send(args),
        "index-pack" => porcelain::index_pack(args),
        "init" => porcelain::init(args),
        "init-db" => porcelain::init_db(args),
        "instaweb" => porcelain::instaweb(args),
        "interpret-trailers" => porcelain::interpret_trailers(args),
        "jump" => porcelain::jump(args),
        "last-modified" => porcelain::last_modified(args),
        "log" => porcelain::log(args),
        "ls-files" => porcelain::ls_files(args),
        "ls-remote" => porcelain::ls_remote(args),
        "ls-tree" => porcelain::ls_tree(args),
        "mailinfo" => porcelain::mailinfo(args),
        "mailsplit" => porcelain::mailsplit(args),
        "maintenance" => porcelain::maintenance(args),
        "merge" => porcelain::merge(args),
        "merge-base" => porcelain::merge_base(args),
        "merge-file" => porcelain::merge_file(args),
        "merge-index" => porcelain::merge_index(args),
        "merge-octopus" => porcelain::merge_octopus(args),
        "merge-one-file" => porcelain::merge_one_file(args),
        "merge-ours" => porcelain::merge_ours(args),
        "merge-recursive" => porcelain::merge_recursive(args),
        "merge-recursive-ours" => porcelain::merge_recursive_ours(args),
        "merge-recursive-theirs" => porcelain::merge_recursive_theirs(args),
        "merge-resolve" => porcelain::merge_resolve(args),
        "merge-subtree" => porcelain::merge_subtree(args),
        "merge-tree" => porcelain::merge_tree(args),
        "mergetool" => porcelain::mergetool(args),
        "mktag" => porcelain::mktag(args),
        "mktree" => porcelain::mktree(args),
        "multi-pack-index" => porcelain::multi_pack_index(args),
        "mv" => porcelain::mv(args),
        "name-rev" => porcelain::name_rev(args),
        "notes" => porcelain::notes(args),
        "p4" => porcelain::p4(args),
        "pack-objects" => porcelain::pack_objects(args),
        "pack-redundant" => porcelain::pack_redundant(args),
        "pack-refs" => porcelain::pack_refs(args),
        "patch-id" => porcelain::patch_id(args),
        "pickaxe" => porcelain::pickaxe(args),
        "prune" => porcelain::prune(args),
        "prune-packed" => porcelain::prune_packed(args),
        "pull" => porcelain::pull(args),
        "push" => porcelain::push(args),
        "quiltimport" => porcelain::quiltimport(args),
        "range-diff" => porcelain::range_diff(args),
        "read-tree" => porcelain::read_tree(args),
        "rebase" => porcelain::rebase(args),
        "receive-pack" => porcelain::receive_pack(args),
        "reflog" => porcelain::reflog(args),
        "refs" => porcelain::refs(args),
        "remote" => porcelain::remote(args),
        "remote-ext" => porcelain::remote_ext(args),
        "remote-fd" => porcelain::remote_fd(args),
        "remote-ftp" => porcelain::remote_ftp(args),
        "remote-ftps" => porcelain::remote_ftps(args),
        "remote-http" => porcelain::remote_http(args),
        "remote-https" => porcelain::remote_https(args),
        "repack" => porcelain::repack(args),
        "replace" => porcelain::replace(args),
        "replay" => porcelain::replay(args),
        "repo" => porcelain::repo(args),
        "request-pull" => porcelain::request_pull(args),
        "rerere" => porcelain::rerere(args),
        "reset" => porcelain::reset(args),
        "restore" => porcelain::restore(args),
        "rev-list" => porcelain::rev_list(args),
        "rev-parse" => porcelain::rev_parse(args),
        "revert" => porcelain::revert(args),
        "rm" => porcelain::rm(args),
        "send-email" => porcelain::send_email(args),
        "send-pack" => porcelain::send_pack(args),
        "sh-i18n--envsubst" => porcelain::sh_i18n__envsubst(args),
        "shell" => porcelain::shell(args),
        "shortlog" => porcelain::shortlog(args),
        "show" => porcelain::show(args),
        "show-branch" => porcelain::show_branch(args),
        "show-index" => porcelain::show_index(args),
        "show-ref" => porcelain::show_ref(args),
        "sparse-checkout" => porcelain::sparse_checkout(args),
        "stage" => porcelain::stage(args),
        "stash" => porcelain::stash(args),
        "status" => porcelain::status(args),
        "stripspace" => porcelain::stripspace(args),
        "submodule" => porcelain::submodule(args),
        "submodule--helper" => porcelain::submodule__helper(args),
        "subtree" => porcelain::subtree(args),
        "switch" => porcelain::switch(args),
        "symbolic-ref" => porcelain::symbolic_ref(args),
        "tag" => porcelain::tag(args),
        "unpack-file" => porcelain::unpack_file(args),
        "unpack-objects" => porcelain::unpack_objects(args),
        "update-index" => porcelain::update_index(args),
        "update-ref" => porcelain::update_ref(args),
        "update-server-info" => porcelain::update_server_info(args),
        "upload-archive" => porcelain::upload_archive(args),
        "upload-archive--writer" => porcelain::upload_archive__writer(args),
        "upload-pack" => porcelain::upload_pack(args),
        "url-parse" => porcelain::url_parse(args),
        "var" => porcelain::var(args),
        "verify-commit" => porcelain::verify_commit(args),
        "verify-pack" => porcelain::verify_pack(args),
        "verify-tag" => porcelain::verify_tag(args),
        "version" => porcelain::version(args),
        "web--browse" => porcelain::web__browse(args),
        "whatchanged" => porcelain::whatchanged(args),
        "worktree" => porcelain::worktree(args),
        "write-tree" => porcelain::write_tree(args),
        // ---- END generated porcelain arms ----

        // An unrecognized verb: a typo or a name this engine has no command for.
        // The CLI never reaches here (it routes unknowns through the external
        // `git-<verb>` / autocorrect path in lib.rs); direct callers like `zrepl`
        // do, so give git's own honest wording rather than implying the verb is a
        // real command merely awaiting a port.
        _ => anyhow::bail!("is not a git command. See 'git --help'."),
    };

    // Per-process activity: for a mutating verb, credit the running process (commit
    // tally on a real HEAD advance, per-verb count otherwise). `commit_head_before`
    // is Some only for commit verbs, so it drives the effective-commit path.
    if track_mutating {
        superset::zppid::note_mutating(sub, commit_head_before.flatten(), attribution);
    }

    // Lock contention → queue, the second half of the rule the pre-flight gate
    // above starts. If the command still failed on a lockfile it could not take
    // (a foreign holder that outlasted the wait), submit it as a job instead of
    // reporting gitoxide's "could not be obtained immediately" — the daemon runs
    // it on the repo's fair FIFO once the lane is free.
    //
    // `ZVCS_QUEUED` marks a job's own re-run: it must NOT re-queue itself, or a
    // permanently stuck lock would spawn jobs forever. There the error stands and
    // the ledger records the failure.
    match result {
        Err(err) if is_lock_verb && !queued_rerun && crate::lock::is_lock_contention(&err) => {
            eprintln!("zvcs: {sub}: index is locked by another writer — queueing");
            superset::queue::queue_verb(sub, args)
        }
        // A ref race takes no lockfile at all: both writers wrote cleanly and this
        // one lost the compare-and-swap on the ref's expected value. Re-running
        // after the winner lands is what fixes it, so it queues like a lock
        // conflict rather than dropping the command.
        Err(err) if is_lock_verb && !queued_rerun && crate::lock::is_ref_race(&err) => {
            eprintln!("zvcs: {sub}: ref moved under another writer — queueing");
            superset::queue::queue_verb(sub, args)
        }
        other => other,
    }
}
