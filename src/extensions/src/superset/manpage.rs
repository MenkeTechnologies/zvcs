//! Man pages for the superset (`z*`) verbs, generated from a structured table.
//!
//! The z-verbs are novel — stock git ships no `git-zsync(1)` — so `git help
//! <zverb>` has nothing to open. This module is the source of truth: one [`Doc`]
//! per verb (name summary, synopsis, description), rendered to a `man(1)` roff
//! page on demand. [`crate::porcelain::help`] materializes the requested page
//! under [`man_dir`] and runs `man -M`, so `git help zverbs` works with no prior
//! setup; [`crate::superset::zdashed`] writes them all up front so `man
//! git-<verb>` resolves once the dir is on `MANPATH`.
//!
//! [`DOCS`] must cover every verb in [`crate::dispatch::SUPERSET_VERBS`]; the
//! `manpage` integration test asserts the two never drift.

use std::io;
use std::path::PathBuf;

/// One verb's manual content. `desc` is a list of paragraphs (one `.PP` apart).
pub struct Doc {
    pub verb: &'static str,
    pub summary: &'static str,
    pub synopsis: &'static str,
    pub desc: &'static [&'static str],
}

/// Every superset verb's manual. Kept in the same order as
/// [`crate::dispatch::SUPERSET_VERBS`]; parity is test-enforced.
pub const DOCS: &[Doc] = &[
    Doc {
        verb: "zsync",
        summary: "reconcile every submodule to its tracked mainline, attached",
        synopsis: "git zsync [<path>...]",
        desc: &[
            "Fetches and fast-forwards every submodule to its tracked mainline (origin/main, else origin/master), leaving HEAD attached to that branch \\(em detached HEAD never happens.",
            "Fast-forward only; a submodule whose worktree is dirty or whose history has diverged is skipped rather than forced. Optional paths limit the operation to the named submodules.",
        ],
    },
    Doc {
        verb: "zbump",
        summary: "forward-only submodule gitlink bumps, then commit them",
        synopsis: "git zbump [<submodule-path>...]",
        desc: &[
            "Advances the parent repository's recorded gitlink for each submodule to that submodule's current HEAD, but only when the new commit is a descendant of the recorded one \\(em a forward-only bump that can never regress a pointer.",
            "The coalesced bumps are then committed, clearing git's \"(new commits)\" status marker. Paths limit the set of submodules considered.",
        ],
    },
    Doc {
        verb: "zdaemon",
        summary: "control the singleton machine-wide coordinator",
        synopsis: "git zdaemon <start|stop|restart|reload|status|info|ping|log>",
        desc: &[
            "Controls the one machine-wide daemon (state under ~/.zvcs, socket ~/.zvcs/zvcs.sock) that replaces git's index.lock with a fair per-repo queue and hosts the async job queue, the SQLite ledger, and reactive autonomy.",
            "start/stop bring it up and down; restart and reload respawn it, re-reading config; status and info report liveness and pid/socket/paths/config; ping is a scriptable liveness check; log [-n N] [-f] shows or tails ~/.zvcs/zvcs.log.",
        ],
    },
    Doc {
        verb: "zrepos",
        summary: "list indexed git repositories",
        synopsis: "git zrepos [<pattern>...]",
        desc: &[
            "Lists every git repository in the machine-wide index, one path per line (pipe-clean), a drop-in for a shell repo-list.",
            "Patterns filter the output by case-insensitive substring match.",
        ],
    },
    Doc {
        verb: "zreindex",
        summary: "(re)crawl for git repositories and refresh the index",
        synopsis: "git zreindex [--sync|--async] [<path>...]",
        desc: &[
            "Crawls for git repositories and records them in the ledger, pruning ones deleted from disk. The walk is parallel and skips mounts that would hang a whole-device scan (kernel pseudo-filesystems, network volumes, the macOS data-volume firmlink).",
            "At a terminal it runs async by default \\(em the crawl detaches and results go to zvcs.log; piped or scripted it runs inline so \"indexed N, pruned M\" stays on stdout. --sync and --async override the default.",
        ],
    },
    Doc {
        verb: "zjobs",
        summary: "list recent async jobs",
        synopsis: "git zjobs [-n <count>]",
        desc: &["Lists recent jobs from the ledger, newest first. -n limits the count."],
    },
    Doc {
        verb: "zjob",
        summary: "show or control an async job",
        synopsis: "git zjob <id> | git zjob <stop|restart> <id>",
        desc: &["Shows one job's ledger record, or stops (cancels) / restarts (re-enqueues) a running or queued job by id."],
    },
    Doc {
        verb: "zcommit",
        summary: "queue an atomic staged-commit job",
        synopsis: "git zcommit [<path>...] -m <msg> [--push]",
        desc: &[
            "Submits a fire-and-forget staged-commit job to the daemon and returns a job number; --push chains a push after the commit.",
            "Falls back to synchronous execution when no daemon is running.",
        ],
    },
    Doc {
        verb: "zpush",
        summary: "queue an async push job with a fast-forward pre-flight",
        synopsis: "git zpush [<refspec>]",
        desc: &["Submits a fire-and-forget push job to the daemon. A network-free / live ls-refs pre-flight refuses a non-fast-forward before the job is enqueued. Falls back to synchronous execution when no daemon is running."],
    },
    Doc {
        verb: "zrepl",
        summary: "interactive console over every zvcs command",
        synopsis: "git zrepl",
        desc: &[
            "Opens an interactive line console. Each line is run exactly as `git <line>` would be, so it drives every dispatchable command \\(em the z* superset verbs and every git-compat porcelain command alike (the latter operating on the current repository), doubling as a live daemon/ledger console.",
            "On a terminal it opens with a stats banner and full line editing: Tab completes the command word against every verb (superset + porcelain), with command history persisted across sessions. Piped stdin falls back to a raw line reader so scripts and heredocs stay usable. Type exit or quit, or press Ctrl-D, to leave.",
        ],
    },
    Doc {
        verb: "zclaim",
        summary: "lease a repository for this session",
        synopsis: "git zclaim [<path>]",
        desc: &["Takes an advisory per-repo lease for the current session (ZVCS_SESSION), refusing if another agent already holds it, so concurrent agents do not collide on the same repository."],
    },
    Doc {
        verb: "zunclaim",
        summary: "release a repository lease",
        synopsis: "git zunclaim [--force] [<path>]",
        desc: &["Releases a lease held on a repository. --force releases a lease held by another session."],
    },
    Doc {
        verb: "zwho",
        summary: "list active claims",
        synopsis: "git zwho",
        desc: &["Lists active claims \\(em which session holds a lease on which repository."],
    },
    Doc {
        verb: "zstatus",
        summary: "working-tree status of indexed repositories",
        synopsis: "git zstatus [--all]",
        desc: &["Reports the current repository's working-tree status live. --all reads every indexed repository's status from the daemon-maintained cache with no filesystem walk."],
    },
    Doc {
        verb: "zlog",
        summary: "machine-wide reflog timeline across indexed repositories",
        synopsis: "git zlog [-n <count>]",
        desc: &["Merges every indexed repository's reflog into one machine-wide timeline, newest first. -n limits the count."],
    },
    Doc {
        verb: "zundo",
        summary: "rewind a repository one reflog step",
        synopsis: "git zundo [<path>]",
        desc: &["Rewinds a repository one reflog step \\(em a reset --hard to the previous HEAD. Refuses on a dirty worktree."],
    },
    Doc {
        verb: "zsnapshot",
        summary: "record the tree's HEADs as a restore point",
        synopsis: "git zsnapshot <name>",
        desc: &["Records the HEAD of the repository and every nested submodule as one named restore point."],
    },
    Doc {
        verb: "zrestore",
        summary: "reset the whole tree back to a snapshot",
        synopsis: "git zrestore <name>",
        desc: &["Resets the whole tree \\(em the repository and every nested submodule \\(em back to a named snapshot."],
    },
    Doc {
        verb: "zsnapshots",
        summary: "list snapshots",
        synopsis: "git zsnapshots",
        desc: &["Lists snapshot names and their repository counts."],
    },
    Doc {
        verb: "zworktree",
        summary: "tree-wide private worktrees",
        synopsis: "git zworktree <add <name>|list|remove <name>>",
        desc: &[
            "add <name> provisions a complete, object-sharing, isolated worktree of the repository plus all nested submodules (each on a zwt/<name> branch) at ~/.zvcs/worktrees/<name>/, so each agent gets a private tree that cannot collide with any other \\(em no re-clone.",
            "list and remove <name> manage them.",
        ],
    },
    Doc {
        verb: "zstash",
        summary: "stash every dirty repo in the tree as one unit",
        synopsis: "git zstash [<name>]",
        desc: &["Parks uncommitted work across every dirty repository in the tree as one named unit."],
    },
    Doc {
        verb: "zunstash",
        summary: "pop the tree-wide stash back",
        synopsis: "git zunstash [<name>]",
        desc: &["Restores (pops) a tree-wide stash, last-in-first-out, applying onto the same commits it was stashed on. A repository whose HEAD has since moved is reported and its stash kept intact."],
    },
    Doc {
        verb: "zstashes",
        summary: "list tree-wide stashes",
        synopsis: "git zstashes",
        desc: &["Lists tree-wide stashes and their repository counts."],
    },
    Doc {
        verb: "zup",
        summary: "reconcile the tree at cwd to latest origin/main",
        synopsis: "git zup [<path>]",
        desc: &["Brings the whole tree \\(em the top-level repository and every nested submodule \\(em to latest origin/main (fetch, then fast-forward, staying attached). A dirty or diverged repository is skipped."],
    },
    Doc {
        verb: "zforeach",
        summary: "run a command across all or a subset of indexed repos",
        synopsis: "git zforeach [<selectors>] -- <command>...",
        desc: &[
            "Runs a command across all indexed repositories, or a subset, in parallel.",
            "Selectors: a bare <pattern> (substring), --repo <p>, --dirty, --ahead, --behind, --claimed, and --session <s>. Everything after -- is the command to run in each selected repository.",
        ],
    },
    Doc {
        verb: "zhook",
        summary: "manage the current repo's ref-change hook",
        synopsis: "git zhook <set <command>|unset|show|list|test>",
        desc: &["Manages and tests the current repository's ref-change hook (zvcs.hook), which the daemon runs on every ref-change when zvcs.autohook is enabled. set installs a command, unset removes it, show/list display it, and test runs it once."],
    },
    Doc {
        verb: "ztrigger",
        summary: "arm any directory to run a command on every ref-change",
        synopsis: "git ztrigger <DIR> <command>... | git ztrigger <list|rm DIR|test DIR>",
        desc: &[
            "The directory-addressed front-end to the hook system: unlike zhook (current repo only) it takes an explicit path, so any repository on the machine can be wired without cd-ing into it, and it flips the master switches itself \\(em no raw git config needed.",
            "git ztrigger DIR CMD writes DIR's local zvcs.hook, indexes DIR, turns on the global zvcs.autohook switch, and reloads the daemon, so CMD runs on every ref-change in DIR. list, rm DIR, and test DIR manage armed triggers.",
        ],
    },
    Doc {
        verb: "zwatch",
        summary: "watch a directory's status without running a command",
        synopsis: "git zwatch <DIR> | git zwatch <list|rm DIR>",
        desc: &["The command-less form of ztrigger: it indexes DIR and turns on zvcs.autostatus so the daemon maintains DIR's cached status on every ref-change, without running any command. list and rm DIR manage watches."],
    },
    Doc {
        verb: "zdashed",
        summary: "install git-<verb> symlinks and man pages",
        synopsis: "git zdashed [<dir>]",
        desc: &[
            "Installs a git-<verb> symlink for every builtin and superset verb into <dir> (default ~/.zvcs/bin), so the dashed external forms resolve to this binary once stock git is removed.",
            "It also writes every extension man page under ~/.zvcs/man, so `man git-<verb>` resolves when that directory is on MANPATH.",
        ],
    },
    Doc {
        verb: "zverbs",
        summary: "list every zvcs extension verb and its usage",
        synopsis: "git zverbs",
        desc: &["Lists every zvcs extension (z*) verb with its one-line usage, sourced from each verb's own -h so the listing can never drift."],
    },
    Doc {
        verb: "zcd",
        summary: "change the working directory (for the zrepl console)",
        synopsis: "git zcd [<dir>|-]",
        desc: &[
            "Changes the process working directory: no argument goes to $HOME, `-` goes to the previous directory ($OLDPWD), and a leading ~ expands to $HOME. OLDPWD and PWD are updated so `zcd -` round-trips, exactly as a shell's cd does.",
            "The zrepl console runs each line in one long-lived process, so a zcd persists across lines \\(em it is what makes the console navigable like a shell. Run standalone it only moves this process's cwd (it cannot change the parent shell's), so it is aimed at the console.",
        ],
    },
    Doc {
        verb: "zpwd",
        summary: "print the working directory",
        synopsis: "git zpwd",
        desc: &["Prints the current working directory. Paired with zcd for shell-like navigation inside the zrepl console."],
    },
    Doc {
        verb: "zls",
        summary: "git-aware directory listing",
        synopsis: "git zls [-alrt] [<path>]",
        desc: &[
            "Lists a directory with a two-column git status field per entry, like `eza --git`: the first column is the staged status (index vs HEAD), the second the unstaged status (worktree vs index). Letters follow eza: N new, M modified, D deleted, R renamed, C copied, T type-change, U conflicted, I ignored, and - unchanged. A directory folds the status of the paths under it, so a subtree with any change is flagged. Outside a git repository the column is omitted.",
            "Flags: -a includes dotfiles, -l is a long listing (permissions, size, relative mtime), -t sorts by modification time (newest first), and -r reverses. The per-path status is the same walk `git status` performs.",
            "On a terminal the output is colored from the same palette eza uses: LS_COLORS for file kinds and *.ext extensions, then EXA_COLORS/EZA_COLORS for the permission bits, size, date, git columns, and punctuation \\(em so it matches the user's eza. NO_COLOR disables color.",
        ],
    },
    Doc {
        verb: "zenv",
        summary: "print, set, or query environment variables",
        synopsis: "git zenv [<NAME=VALUE>...|<NAME>...]",
        desc: &[
            "With no arguments, prints every environment variable as NAME=VALUE, sorted. A NAME=VALUE argument sets that variable; a bare NAME prints its value (nothing if unset).",
            "In the zrepl console a variable set with zenv persists for every later `git` line \\(em set GIT_AUTHOR_NAME, ZVCS_SESSION, and the like once and the whole session sees it.",
        ],
    },
    Doc {
        verb: "zunset",
        summary: "remove environment variables",
        synopsis: "git zunset <NAME>...",
        desc: &["Removes one or more environment variables from the process, the complement of `zenv NAME=VALUE`. In the zrepl console the change persists for later `git` lines."],
    },
    Doc {
        verb: "zecho",
        summary: "print arguments joined by a space",
        synopsis: "git zecho [-n] [<arg>...]",
        desc: &["Prints its arguments joined by a single space. A leading -n suppresses the trailing newline. Arguments are printed literally \\(em there is no shell variable or glob expansion."],
    },
    Doc {
        verb: "zdoctor",
        summary: "health check of the zvcs environment",
        synopsis: "git zdoctor",
        desc: &[
            "Runs a set of environment checks and prints each as OK, WARN, or FAIL: whether this binary is the git on PATH, whether $ZVCS_HOME exists, whether the coordinator daemon is running, whether a ledger exists, how many man pages are installed, whether ~/.zvcs/man is on MANPATH, and whether the dashed git-<verb> symlinks are installed.",
            "The process exits non-zero only when a hard FAIL is found, so it is usable in scripts and CI.",
        ],
    },
    Doc {
        verb: "zmkdir",
        summary: "create directories",
        synopsis: "git zmkdir [-p] <dir>...",
        desc: &["Creates each named directory. -p creates any missing parent directories and does not error if the directory already exists. A native filesystem convenience for the zrepl console."],
    },
    Doc {
        verb: "ztouch",
        summary: "create files or update their mtime",
        synopsis: "git ztouch <file>...",
        desc: &["Creates each named file if it does not exist (leaving existing contents intact) and updates its modification time to now, like the shell's touch."],
    },
    Doc {
        verb: "zrm",
        summary: "remove files and directories",
        synopsis: "git zrm [-r] [-f] <path>...",
        desc: &["Removes files from the filesystem \\(em this is not `git rm` (which stages a removal in the index); it deletes on disk. -r removes directories recursively, -f ignores paths that do not exist. Symlinks are removed, never followed."],
    },
    Doc {
        verb: "zcp",
        summary: "copy files and directories",
        synopsis: "git zcp [-r] <src>... <dst>",
        desc: &["Copies files; -r copies directories recursively. With several sources, <dst> must be an existing directory and each source is copied into it under its own name; with one source and a non-directory <dst>, <dst> is the copy's name."],
    },
    Doc {
        verb: "zmv",
        summary: "move or rename files and directories",
        synopsis: "git zmv <src>... <dst>",
        desc: &["Moves or renames paths. With several sources, <dst> must be an existing directory. A rename is used when possible, falling back to copy-then-remove when the move crosses filesystems. This is not `git mv` (which also updates the index); it only moves on disk."],
    },
    Doc {
        verb: "zcat",
        summary: "print file contents",
        synopsis: "git zcat <file>...",
        desc: &["Writes each named file's bytes to stdout, in order, like the shell's cat."],
    },
    Doc {
        verb: "zln",
        summary: "create a hard link or symlink",
        synopsis: "git zln [-s] <target> <link>",
        desc: &["Creates <link> pointing at <target>: a hard link by default, or a symbolic link with -s."],
    },
    Doc {
        verb: "zheads",
        summary: "HEAD of every indexed repo, in parallel",
        synopsis: "git zheads [selectors]",
        desc: &[
            "Prints each indexed repository's checked-out branch (or (detached)/(unborn)), short HEAD id, and a * when the worktree has tracked changes \\(em a one-glance view of where every repo in the machine sits.",
            "Selectors are the same as zforeach: bare <pattern>, --repo <p>, --dirty, --ahead, --behind, --claimed, --session <s>. The probe is a native gix read run across all repos on a bounded worker pool.",
        ],
    },
    Doc {
        verb: "zdirty",
        summary: "list indexed repos with tracked changes",
        synopsis: "git zdirty [selectors]",
        desc: &["Lists only the indexed repositories whose worktree has uncommitted tracked changes (the same \"dirty\" gix reports for zstatus; an untracked-only repo counts as clean). Scanned in parallel. Selectors narrow the set as in zforeach."],
    },
    Doc {
        verb: "zbranches",
        summary: "local branches of every indexed repo",
        synopsis: "git zbranches [selectors]",
        desc: &["Prints each indexed repository's local branch names, grouped by repo, scanned in parallel."],
    },
    Doc {
        verb: "ztags",
        summary: "tag count of every indexed repo",
        synopsis: "git ztags [selectors]",
        desc: &["Prints how many tags each indexed repository has, scanned in parallel."],
    },
    Doc {
        verb: "zremotes",
        summary: "remotes and URLs of every indexed repo",
        synopsis: "git zremotes [selectors]",
        desc: &["Prints each indexed repository's remotes and their fetch URLs, grouped by repo, scanned in parallel."],
    },
    Doc {
        verb: "zsize",
        summary: "on-disk .git size of every indexed repo",
        synopsis: "git zsize [selectors]",
        desc: &["Prints each indexed repository's on-disk .git size, largest first, with a total, so the heaviest repos in the tree are obvious. Sizes are summed with a native filesystem walk in parallel."],
    },
    Doc {
        verb: "zage",
        summary: "HEAD commit age of every indexed repo",
        synopsis: "git zage [selectors]",
        desc: &["Prints how long ago each indexed repository's HEAD commit was made (a relative time like `3 days ago`), scanned in parallel \\(em a quick read of which repos are stale."],
    },
    Doc {
        verb: "zpull",
        summary: "parallel fetch + fast-forward of every indexed repo",
        synopsis: "git zpull [selectors]",
        desc: &[
            "Fetches and fast-forwards every selected indexed repository to its tracked mainline, in parallel, using the same native ff-only reconcile as zsync: a dirty or diverged repo is reported and skipped, never forced.",
            "Where zsync/zup act on the current submodule tree, zpull acts on the machine-wide index, so selectors (--repo, --behind, …) scope which repos are pulled.",
        ],
    },
    Doc {
        verb: "zgrep",
        summary: "parallel content search across indexed repos",
        synopsis: "git zgrep [selectors] [-i] <pattern>",
        desc: &[
            "Searches the tracked file content of every indexed repository for <pattern> (a regular expression) in parallel, printing path:line:text for each match. -i is case-insensitive; binary files are skipped, as git grep does.",
            "Only tracked, non-conflicted worktree files are searched. Selectors narrow the repo set as in zforeach.",
        ],
    },
    Doc {
        verb: "zahead",
        summary: "indexed repos ahead of their upstream",
        synopsis: "git zahead [selectors]",
        desc: &["Lists the indexed repositories that have commits not yet on their configured upstream, with the count, computed in parallel. Repos with no upstream are omitted."],
    },
    Doc {
        verb: "zbehind",
        summary: "indexed repos behind their upstream",
        synopsis: "git zbehind [selectors]",
        desc: &["Lists the indexed repositories whose upstream has commits they lack, with the count, computed in parallel. Repos with no upstream are omitted."],
    },
    Doc {
        verb: "zauthors",
        summary: "commit counts by author across indexed repos",
        synopsis: "git zauthors [selectors]",
        desc: &["Walks every indexed repository's HEAD history in parallel, tallies commits by author `Name <email>`, aggregates across all repos, and prints them ranked by count \\(em a machine-wide contribution summary."],
    },
    Doc {
        verb: "zhot",
        summary: "indexed repos ranked by recent activity",
        synopsis: "git zhot [selectors] [<days>]",
        desc: &["Ranks indexed repositories by the number of commits made in the last <days> (default 30), most active first, counted in parallel \\(em a quick read of where work is happening across the tree."],
    },
    Doc {
        verb: "zconflicts",
        summary: "indexed repos mid-operation or conflicted",
        synopsis: "git zconflicts [selectors]",
        desc: &["Lists the indexed repositories that are in the middle of a merge, rebase, cherry-pick, revert, or bisect, or that have unmerged (conflicted) index entries, with the operation(s) named \\(em so a stuck repo among many is found at a glance."],
    },
    Doc {
        verb: "zfetch",
        summary: "parallel git fetch across indexed repos",
        synopsis: "git zfetch [selectors]",
        desc: &["Runs `git fetch` in every selected indexed repository concurrently, through this binary's own porcelain and fair per-repo lane. Output is grouped per repo and failures are recorded in the ledger."],
    },
    Doc {
        verb: "zgc",
        summary: "parallel git gc across indexed repos",
        synopsis: "git zgc [selectors]",
        desc: &["Runs `git gc` in every selected indexed repository concurrently \\(em machine-wide maintenance in one command."],
    },
    Doc {
        verb: "zfsck",
        summary: "parallel git fsck across indexed repos",
        synopsis: "git zfsck [selectors]",
        desc: &["Runs `git fsck` in every selected indexed repository concurrently, to check object integrity across the whole tree at once."],
    },
    Doc {
        verb: "zprune",
        summary: "parallel git prune across indexed repos",
        synopsis: "git zprune [selectors]",
        desc: &["Runs `git prune` in every selected indexed repository concurrently, removing unreachable objects tree-wide."],
    },
    Doc {
        verb: "zcheckout",
        summary: "check out a branch in every repo that has it",
        synopsis: "git zcheckout [selectors] <branch>",
        desc: &["Checks out `<branch>` in every selected indexed repository that already has it, in parallel. A repo without the branch is skipped \\(em the branch is never created \\(em so this is a safe way to move a whole tree onto a shared branch name."],
    },
    Doc {
        verb: "ztagall",
        summary: "create a tag at HEAD in every indexed repo",
        synopsis: "git ztagall [selectors] <tag>",
        desc: &["Creates tag `<tag>` at HEAD in every selected indexed repository, in parallel. A repo that already has the tag reports the failure rather than moving it."],
    },
    Doc {
        verb: "zcommitall",
        summary: "commit tracked changes across every dirty repo",
        synopsis: "git zcommitall [selectors] -m <msg>",
        desc: &["Commits tracked changes (`git commit -a`) with message `<msg>` in every selected indexed repository whose worktree is dirty, in parallel. Clean repos are skipped. Untracked files are not staged (as with `commit -a`)."],
    },
    Doc {
        verb: "zpushall",
        summary: "push every indexed repo that is ahead",
        synopsis: "git zpushall [selectors]",
        desc: &["Runs `git push` in every selected indexed repository that is ahead of its upstream, in parallel. Repos that are not ahead (or have no upstream) are skipped, so no needless network calls are made."],
    },
    Doc {
        verb: "zclean",
        summary: "remove untracked files across indexed repos",
        synopsis: "git zclean -f [selectors]",
        desc: &["Runs `git clean -fd` \\(em remove untracked files and directories \\(em in every selected indexed repository, in parallel. Because it deletes, the `-f` flag is required; ignored files are left (as `git clean` does without `-x`)."],
    },
    Doc {
        verb: "zwait",
        summary: "block until a repo's async jobs drain",
        synopsis: "git zwait [<path>]",
        desc: &["Blocks until the repository at `<path>` (or the current directory) has no queued or running async jobs (`zcommit`/`zpush`) left in the ledger \\(em the join for that repo's fire-and-forget work. With no daemon there are no jobs, so it returns at once."],
    },
    Doc {
        verb: "zqueue",
        summary: "list queued and running async jobs",
        synopsis: "git zqueue",
        desc: &["Lists the async jobs currently queued or running in the ledger (id, state, kind, repo) \\(em what the daemon is working through right now."],
    },
    Doc {
        verb: "zbarrier",
        summary: "block until the async queue is idle",
        synopsis: "git zbarrier",
        desc: &["Blocks until the entire async job queue is idle \\(em every repository's queued and running jobs have drained \\(em the global join after a burst of `zcommit`/`zpush`."],
    },
];

/// The manual for `verb`, or `None` if it is not a superset verb.
pub fn doc_for(verb: &str) -> Option<&'static Doc> {
    DOCS.iter().find(|d| d.verb == verb)
}

/// Root of the generated man tree: `$ZVCS_HOME/man` (honors `ZVCS_HOME`). Pages
/// live under `man1/` within it, so `man -M <this>` finds `git-<verb>(1)`.
pub fn man_dir() -> PathBuf {
    crate::superset::zdaemon::zvcs_home().join("man")
}

/// Render `doc` to a `man(1)` roff page.
pub fn roff(doc: &Doc) -> String {
    let title = format!("git-{}", doc.verb).to_uppercase();
    let mut s = String::new();
    s.push_str(&format!(".TH \"{title}\" 1 \"\" \"zvcs\" \"zvcs Manual\"\n"));
    s.push_str(".SH NAME\n");
    s.push_str(&format!("git\\-{} \\- {}\n", doc.verb, esc(doc.summary)));
    s.push_str(".SH SYNOPSIS\n.nf\n");
    s.push_str(&format!("\\fI{}\\fR\n", esc(doc.synopsis)));
    s.push_str(".fi\n");
    s.push_str(".SH DESCRIPTION\n");
    for (i, para) in doc.desc.iter().enumerate() {
        if i > 0 {
            s.push_str(".PP\n");
        }
        s.push_str(&esc_line(para));
        s.push('\n');
    }
    s.push_str(".SH SEE ALSO\n");
    s.push_str("\\fBgit\\-zverbs\\fR(1), \\fBgit\\-help\\fR(1)\n");
    s
}

/// Escape roff-significant characters in inline text. Only the backslash needs
/// escaping in body prose (`\(em` etc. are intentional and pre-written); a
/// literal backslash would otherwise start an escape.
fn esc(text: &str) -> String {
    // Preserve intentional `\(em` and `\-` sequences: those are the only
    // backslashes the table ever contains, so no blanket doubling is applied.
    text.to_string()
}

/// Guard a paragraph against being read as a roff control line: a line starting
/// with `.` or `'` is a request, so prefix a zero-width `\&`.
fn esc_line(text: &str) -> String {
    let e = esc(text);
    if e.starts_with('.') || e.starts_with('\'') {
        format!("\\&{e}")
    } else {
        e
    }
}

/// Write a single verb's page under `man_dir()/man1/`, returning the man root
/// (for `man -M`). `None` if `verb` is not a superset verb.
pub fn ensure_page(verb: &str) -> io::Result<Option<PathBuf>> {
    let Some(doc) = doc_for(verb) else {
        return Ok(None);
    };
    let dir = man_dir().join("man1");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("git-{}.1", doc.verb)), roff(doc))?;
    Ok(Some(man_dir()))
}

/// Write every superset verb's page under `man_dir()/man1/`, returning the count.
pub fn install_all() -> io::Result<usize> {
    let dir = man_dir().join("man1");
    std::fs::create_dir_all(&dir)?;
    for doc in DOCS {
        std::fs::write(dir.join(format!("git-{}.1", doc.verb)), roff(doc))?;
    }
    Ok(DOCS.len())
}
