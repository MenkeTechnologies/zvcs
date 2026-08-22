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
            "start/stop bring it up and down; restart and reload respawn it, re-reading config; status and info report liveness and pid/socket/paths/config; ping is a scriptable liveness check; log [-n N] [-f] shows or tails ~/.zvcs/zvcs.log, whose every line opens with a local RFC-3339 stamp (2026-08-20T14:03:11-07:00) ahead of its [zvcs ...] tag.",
        ],
    },
    Doc {
        verb: "zconfig",
        summary: "inspect and toggle the daemon's [zvcs] feature switches",
        synopsis: "git zconfig [<name> [on|off|<count>|default]]",
        desc: &[
            "Turns the daemon's autonomy features on and off from the CLI instead of hand-editing ~/.gitconfig. With no argument it lists every setting, its effective value, and whether it is set in your config (marked *) or showing the built-in default.",
            "The boolean switches are autoreconcile (auto-sync submodules to origin/main on change), autobump (forward-only gitlink bumps + commit), autocrawl (crawl zvcs.crawlroots into the index on daemon start), autostatus (recompute a repo's status cache when it changes), autohook (fire each repo's zvcs.hook on change), and autodups (fan a commit out to local duplicate checkouts) \\(em all off by default. Set them with `git zconfig <name> on|off`.",
            "The numeric knobs are statusinterval (status-cache backstop sweep in seconds, default 10, 0 disables), watchmru (file-watch the N most-recently-used repos, default 512, 0 disables), and interval (autonomy debounce in seconds, default 30). Set them with `git zconfig <name> <count>`.",
            "`git zconfig <name>` shows a single setting; `git zconfig <name> default` reverts it to the built-in default; `git zconfig all on|off` flips every autonomy switch at once (off also zeroes the two background loops so the daemon idles).",
            "Writes go to the global config (~/.gitconfig), the file the daemon reads. A change reloads a running daemon automatically so it takes effect at once; if no daemon is running the new value applies on its next start (toggling never spawns one).",
        ],
    },
    Doc {
        verb: "zrepos",
        summary: "list indexed git repositories",
        synopsis: "git zrepos [<pattern>...] [--json]",
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
        synopsis: "git zjobs [-n <count>] [--json]",
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
        verb: "zsubmit",
        summary: "run an arbitrary command as an async daemon job",
        synopsis: "git zsubmit [--] <command> [args...]",
        desc: &[
            "Ships an arbitrary command to the daemon's worker pool as an async job and prints the job id. Track it with `git zjobs` and `git zjob <id>` (which shows its state and captured output), and cancel it with `git zjob stop <id>` \\(em the same ledger and controls as zcommit/zpush.",
            "The command runs in the current directory (a git repository is not required \\(em a repo-less job simply records with no repo) with no shell, so submit `sh -c \"...\"` when you need pipes, redirects, or globbing. It runs with the submitter's carried git-identity environment; otherwise it inherits the daemon's. With no daemon running, it executes inline.",
        ],
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
        verb: "zbanner",
        summary: "reprint the zrepl startup banner",
        synopsis: "git zbanner [--color|--no-color]",
        desc: &[
            "Prints the banner git zrepl shows at startup: the zvcs logo, then a box of live stats \\(em os/arch/pid, core count, indexed repository count, and the superset/git-compat/total verb counts.",
            "Every number is read at call time, so re-running it inside a console reflects the tree as it is now (repos indexed since the console opened, a newer build's verb counts) rather than as it was at startup. The repository count comes from the ledger and reads as \\(em when no index exists yet.",
            "Color is on for a terminal unless NO_COLOR is set; --color and --no-color (alias --mono) force it either way, so the ANSI form can be redirected to a file and the plain form kept on a tty.",
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
        synopsis: "git zwho [--json]",
        desc: &["Lists active claims \\(em which session holds a lease on which repository."],
    },
    Doc {
        verb: "zstatus",
        summary: "working-tree status of indexed repositories",
        synopsis: "git zstatus [--all] [--json]",
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
            "remove tears down the worktree tree and, in each repository, the linked-worktree metadata it provisioned \\(em identified by the round trip between the worktree's .git pointer and that metadata's own gitdir file. A pointer that is unreadable, malformed, symlinked, or names anything else is refused by name on stderr and left on disk, and the command exits non-zero rather than deleting a path it cannot prove it wrote.",
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
            "Selectors: a bare <pattern> (substring), --repo <p>, --dirty, --ahead, --behind, --claimed, and --session <s>. Everything after -- is the command to run in each selected repository. See `git help zselectors` for the full selector grammar.",
        ],
    },
    Doc {
        verb: "zselectors",
        summary: "the shared selector grammar the parallel verbs accept",
        synopsis: "git zselectors",
        desc: &[
            "Many superset verbs take an optional leading [selectors] argument that narrows which indexed repositories they act on \\(em zforeach, zheads, zdirty, zpull, zgrep, zfetch, zgc, zcommitall, and the rest. This page documents that shared grammar; `git zselectors` prints a short version to the terminal.",
            "With no selector a verb acts on EVERY indexed repository. The selectors are: a bare <pattern>, a case-insensitive substring matched against each repo's working-tree path (repeatable \\(em all patterns must match); --repo <pattern>, the same thing spelled out; --dirty, repos with uncommitted tracked changes; --ahead and --behind, repos ahead of or behind their upstream; --claimed, repos with an active lease (git zclaim); and --session <s>, repos leased by session <s>.",
            "Selectors COMPOSE with AND \\(em a repo must match every selector given (so --ahead --behind is empty, since a repo is never both). --dirty, --ahead, and --behind read the daemon's status cache (kept warm by the status maintainer), so they reflect the last scan rather than a live check.",
            "Examples: `git zheads` (all repos) vs `git zheads cask` (only repos whose path contains \"cask\"); `git zforeach --dirty -- git status` runs git status in every dirty repo; `git zpull --behind` fast-forwards only the repos behind their upstream; `git zgrep --repo web TODO` searches \"TODO\" in repos matching \"web\".",
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
        verb: "zshadow",
        summary: "install the ~/.zvcs shadow and print the shell lines for it",
        synopsis: "git zshadow [<dir>] [-n|--print] [--all]",
        desc: &[
            "Sets up the whole shadow install in one command: a `git` symlink to this binary in <dir> (default ~/.zvcs/bin), a git-<verb> dashed link beside it for every verb the dispatcher serves, every superset man page under ~/.zvcs/man, the HTML documentation set under ~/.zvcs/share/doc/git-doc (what `git help -w <cmd>` opens for every page git's own installed HTML manual does not hold), and the zvcs-forked zsh completion as ~/.zvcs/completions/_git. Everything installed is compiled into the binary, so nothing is needed from the source tree.",
            "stdout carries shell code only \\(em the export PATH line for the bin directory, the export MANPATH line for the man directory, and the fpath line for the completion directory \\(em so `eval \"$(git zshadow)\"` sets the current shell up and the same lines paste into ~/.zshrc. The install summary goes to stderr, where an eval leaves it alone.",
            "A PATH or MANPATH line the environment already satisfies is printed commented out, so re-evaluating never duplicates an entry and the line is still visible to uncomment when pasting into an rc file; --all prints every line uncommented. fpath is a zsh variable rather than an exported one, so it cannot be inspected from here and its line is always live \\(em put it before compinit, and `typeset -U fpath` keeps repeats harmless.",
            "-n (--print) prints the lines without installing anything. Paths under $HOME are written as $HOME/... so the lines are portable across machines.",
            "Homebrew installs this binary as `zvcs` rather than `git`, so the bootstrap there is `zvcs zshadow` \\(em the same command under its other name. Once the printed PATH line is in effect, `git` is this binary and `git zshadow` is the way to re-run it, which a brew upgrade calls for: the shim points at the binary that installed it.",
            "Idempotent: a correct symlink is left alone, a stale one is repointed, a real file of the same name is never clobbered, and the completion is rewritten only when its content differs.",
        ],
    },
    Doc {
        verb: "zdashed",
        summary: "install git-<verb> symlinks and man pages",
        synopsis: "git zdashed [<dir>]",
        desc: &[
            "Installs a git-<verb> symlink for every builtin and superset verb into <dir> (default ~/.zvcs/bin), so the dashed external forms resolve to this binary once stock git is removed.",
            "It also writes every extension man page under ~/.zvcs/man, so `man git-<verb>` resolves when that directory is on MANPATH, and the HTML documentation set under ~/.zvcs/share/doc/git-doc, so `git help -w <cmd>` finds a page for every command without generating one first, on a host whose git installation ships no HTML manual of its own.",
        ],
    },
    Doc {
        verb: "zverbs",
        summary: "list every zvcs extension verb and its usage",
        synopsis: "git zverbs [--json]",
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
        synopsis: "git zheads [selectors] [--json]",
        desc: &[
            "Prints each indexed repository's checked-out branch (or (detached)/(unborn)), short HEAD id, and a * when the worktree has tracked changes \\(em a one-glance view of where every repo in the machine sits.",
            "Selectors are the same as zforeach: bare <pattern>, --repo <p>, --dirty, --ahead, --behind, --claimed, --session <s>. The probe is a native gix read run across all repos on a bounded worker pool.",
        ],
    },
    Doc {
        verb: "zdirty",
        summary: "list indexed repos with tracked changes",
        synopsis: "git zdirty [selectors] [--json]",
        desc: &["Lists only the indexed repositories whose worktree has uncommitted tracked changes (the same \"dirty\" gix reports for zstatus; an untracked-only repo counts as clean). Scanned in parallel. Selectors narrow the set as in zforeach."],
    },
    Doc {
        verb: "zbranches",
        summary: "local branches of every indexed repo",
        synopsis: "git zbranches [selectors] [--json]",
        desc: &["Prints each indexed repository's local branch names, grouped by repo, scanned in parallel."],
    },
    Doc {
        verb: "ztags",
        summary: "tag count of every indexed repo",
        synopsis: "git ztags [selectors] [--json]",
        desc: &["Prints how many tags each indexed repository has, scanned in parallel."],
    },
    Doc {
        verb: "zremotes",
        summary: "remotes and URLs of every indexed repo",
        synopsis: "git zremotes [selectors] [--json]",
        desc: &["Prints each indexed repository's remotes and their fetch URLs, grouped by repo, scanned in parallel."],
    },
    Doc {
        verb: "zsize",
        summary: "on-disk .git size of every indexed repo",
        synopsis: "git zsize [selectors] [--json]",
        desc: &["Prints each indexed repository's on-disk .git size, largest first, with a total, so the heaviest repos in the tree are obvious. Sizes are summed with a native filesystem walk in parallel."],
    },
    Doc {
        verb: "zage",
        summary: "HEAD commit age of every indexed repo",
        synopsis: "git zage [selectors] [--json]",
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
        verb: "zattach",
        summary: "re-attach every detached-HEAD indexed repo to its mainline",
        synopsis: "git zattach [selectors]",
        desc: &[
            "Re-attaches every selected indexed repository that is on a detached HEAD to its mainline branch (main, else master), fleet-wide \\(em the kill-switch for the detached HEADs `git submodule update` leaves behind across the whole index.",
            "It uses the same local, no-clobber attach zsync applies to submodules: it never contacts a remote, never moves the checked-out commit, and never touches the worktree or index \\(em it only creates or fast-forwards the local mainline branch to the commit HEAD is already at and makes HEAD symbolic to it, so it is safe even on a dirty repo. A repo whose local mainline is ahead of or diverged from HEAD is refused rather than clobbered (catching the worktree up is zpull/zsync's clean-only job); a repo with no main or master anywhere is skipped. It reports how many were attached, already on a branch, had no mainline, or were refused.",
        ],
    },
    Doc {
        verb: "zgrep",
        summary: "parallel content search across indexed repos",
        synopsis: "git zgrep [selectors] [-i] <pattern> [--json]",
        desc: &[
            "Searches the tracked file content of every indexed repository for <pattern> (a regular expression) in parallel, printing path:line:text for each match. -i is case-insensitive; binary files are skipped, as git grep does.",
            "Only tracked, non-conflicted worktree files are searched. Selectors narrow the repo set as in zforeach.",
        ],
    },
    Doc {
        verb: "zahead",
        summary: "indexed repos ahead of their upstream",
        synopsis: "git zahead [selectors] [--json]",
        desc: &["Lists the indexed repositories that have commits not yet on their configured upstream, with the count, computed in parallel. Repos with no upstream are omitted."],
    },
    Doc {
        verb: "zbehind",
        summary: "indexed repos behind their upstream",
        synopsis: "git zbehind [selectors] [--json]",
        desc: &["Lists the indexed repositories whose upstream has commits they lack, with the count, computed in parallel. Repos with no upstream are omitted."],
    },
    Doc {
        verb: "zunpushed",
        summary: "per-repo unpushed commits (the detailed zahead)",
        synopsis: "git zunpushed [selectors] [--json]",
        desc: &["Lists, per indexed repository, the actual commits HEAD has that its upstream lacks \\(em short id and summary line, grouped under the repo path \\(em computed in parallel. Where `zahead` gives only a count, this shows what is unpushed. The list is capped per repo so a long divergence cannot flood the output; repos with no upstream or nothing unpushed are omitted."],
    },
    Doc {
        verb: "zunpulled",
        summary: "per-repo commits the upstream has that local lacks",
        synopsis: "git zunpulled [selectors] [--json]",
        desc: &["Lists, per indexed repository, the commits on the configured upstream that the local branch lacks \\(em short id and summary, grouped per repo. The detailed form of `zbehind`. A `git zfetch` first makes the upstream refs current. Repos with no upstream or nothing unpulled are omitted."],
    },
    Doc {
        verb: "zauthors",
        summary: "commit counts by author across indexed repos",
        synopsis: "git zauthors [selectors] [--json]",
        desc: &["Walks every indexed repository's HEAD history in parallel, tallies commits by author `Name <email>`, aggregates across all repos, and prints them ranked by count \\(em a machine-wide contribution summary."],
    },
    Doc {
        verb: "zhot",
        summary: "indexed repos ranked by recent activity",
        synopsis: "git zhot [selectors] [<days>] [--json]",
        desc: &["Ranks indexed repositories by the number of commits made in the last <days> (default 30), most active first, counted in parallel \\(em a quick read of where work is happening across the tree."],
    },
    Doc {
        verb: "zconflicts",
        summary: "indexed repos mid-operation or conflicted",
        synopsis: "git zconflicts [selectors] [--json]",
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
        verb: "zreset",
        summary: "parallel git reset across indexed repos",
        synopsis: "git zreset [selectors] [--soft|--mixed|--hard] [<ref>]",
        desc: &["Runs `git reset` in every selected indexed repository concurrently. The mode flag and optional `<ref>` pass straight through to git, so the default (as git's) is `--mixed HEAD`, which unstages. `--hard` discards worktree changes across the whole selection \\(em narrow it with selectors (e.g. `--dirty`) first."],
    },
    Doc {
        verb: "zabort",
        summary: "abort an in-progress operation across mid-op repos",
        synopsis: "git zabort [selectors]",
        desc: &["Aborts an in-progress merge, rebase, cherry-pick, or revert in every selected indexed repository that is mid-operation \\(em running the matching `--abort` \\(em and skips repos with nothing in progress. Cleans up after a fan-out (or a many-agent session) that left repos stuck in a half-finished operation."],
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
    Doc {
        verb: "zstale",
        summary: "indexed repos not committed to in a while",
        synopsis: "git zstale [selectors] [<days>] [--json]",
        desc: &["Lists the indexed repositories whose HEAD commit is older than <days> (default 90), with how long ago \\(em surfacing abandoned repos across the tree. Scanned in parallel."],
    },
    Doc {
        verb: "zlast",
        summary: "indexed repos by most recent commit",
        synopsis: "git zlast [selectors] [--json]",
        desc: &["Lists indexed repositories ordered by HEAD commit time, most recently committed first, each with its relative age \\(em where work happened most recently."],
    },
    Doc {
        verb: "zbig",
        summary: "largest tracked files across indexed repos",
        synopsis: "git zbig [selectors] [<n>] [--json]",
        desc: &["Lists the largest tracked files across every indexed repository, top <n> (default 20), by on-disk size \\(em bloat hunting tree-wide. Files are gathered in parallel."],
    },
    Doc {
        verb: "zfiles",
        summary: "tracked file count per indexed repo",
        synopsis: "git zfiles [selectors] [--json]",
        desc: &["Prints the number of tracked files in each indexed repository, largest first, scanned in parallel."],
    },
    Doc {
        verb: "zcommits",
        summary: "HEAD-history commit count per indexed repo",
        synopsis: "git zcommits [selectors] [--json]",
        desc: &["Counts the commits reachable from HEAD in each indexed repository \\(em its history depth \\(em and prints them deepest first, walked in parallel."],
    },
    Doc {
        verb: "zpristine",
        summary: "indexed repos that are clean and in sync",
        synopsis: "git zpristine [selectors] [--json]",
        desc: &["Lists the indexed repositories that need nothing: no uncommitted changes, on a branch (not detached), and in sync with their upstream (or having none configured). The complement of `zdirty`/`zahead`/`zbehind`/`zconflicts` \\(em the green set \\(em computed in parallel."],
    },
    Doc {
        verb: "zdivergent",
        summary: "indexed repos diverged from upstream",
        synopsis: "git zdivergent [selectors] [--json]",
        desc: &["Lists indexed repositories that are both ahead of and behind their upstream \\(em history has forked, so a merge or rebase is needed. Computed in parallel."],
    },
    Doc {
        verb: "zorphans",
        summary: "indexed repos with no remote",
        synopsis: "git zorphans [selectors] [--json]",
        desc: &["Lists indexed repositories with no remote configured \\(em nothing to fetch from or push to. Scanned in parallel."],
    },
    Doc {
        verb: "zsessions",
        summary: "active sessions ranked by repos held",
        synopsis: "git zsessions [--json]",
        desc: &["Lists the sessions that currently hold claims, ranked by how many repositories each holds \\(em which agent is working the most of the tree."],
    },
    Doc {
        verb: "zidle",
        summary: "indexed repos with no active claim",
        synopsis: "git zidle [selectors]",
        desc: &["Lists indexed repositories that no session has claimed \\(em the ones free for an agent to pick up. Selectors narrow the candidate set as in zforeach."],
    },
    Doc {
        verb: "zdashboard",
        summary: "live tiled TUI: fleet, events, and commands on one screen",
        synopsis: "git zdashboard [--once] [--json]",
        desc: &[
            "A live, tiled dashboard of the whole indexed tree on one screen: a header of aggregate totals (dirty, ahead, behind, diverged, detached, clean, claims, sessions, queue, and whether the daemon is up) over four tiles \\(em a FLEET table (every repo, most recently active first, with the on-screen rows' HEAD state live-refreshed so nothing is stale), a PROCESSES tile (each committing ppid with its command, cwd, commit tally, live/dead state and last-commit age, as zppid shows \\(em live processes first, then most commits), an EVENTS feed (commits, reconciles, status changes, as zevents shows), and a COMMANDS feed (every git command run across the machine, with the agent that ran it, as zcommands shows). Press q or Esc to quit.",
            "Every tile reads the daemon-maintained status cache and ledger, not a live per-repo walk, so the dashboard stays responsive across thousands of repos. The COMMANDS tile is populated once command logging is on (`git zcommands` enables it).",
            "For scripting, `--once` or `--json` (or a non-terminal stdout) prints the instant text summary instead of the TUI \\(em the same aggregate counts, one screen, no interaction.",
        ],
    },
    Doc {
        verb: "ztop",
        summary: "live htop-style fleet monitor, most churn on top",
        synopsis: "git ztop [selectors] [--interval <secs>] [--once] [--mono]",
        desc: &[
            "A full-screen, live monitor of the whole indexed tree \\(em an htop for the fleet. A header shows the totals (dirty, ahead, behind, diverged, detached, clean, claims, sessions, queue depth, and whether the daemon is up); a table lists every repo with a churn bar, path, HEAD, state, and how long ago it last changed.",
            "Repos are sorted with the most churn on top: when a repo's HEAD or dirty flag changes between frames (the daemon rewrote its status row), its churn score bumps and then decays, so whatever is moving right now rises to the top, like an htop CPU column. Press s to cycle the sort (churn / state / name), the arrows / PgUp / PgDn / g / G to scroll, and q (or Esc) to quit.",
            "Each frame reads the daemon-maintained status cache, not a live per-repo scan, so it stays responsive across thousands of repos \\(em start the daemon (git zdaemon start) so the cache stays warm and churn reflects real activity. Selectors narrow the view to a subset; --interval sets the refresh seconds, --mono (or NO_COLOR) drops color, and --once (or a non-terminal stdout) prints a single plain-text frame for scripts.",
        ],
    },
    Doc {
        verb: "zevents",
        summary: "one live feed of commits/reconciles/status-changes across the tree",
        synopsis: "git zevents [-n <count>] [--kind commit|stage|status|reconcile] [--repo <substr>] [--json] [--no-follow]",
        desc: &[
            "Prints a single structured, live feed of what is happening across the whole indexed tree: commits (a HEAD moved), status-changes (a dirty or sync transition), and reconciles (a fetch-free fast-forward the daemon applied). It shows a short backlog, then follows live.",
            "Events come from the index's append-only events table, written by triggers on the status cache that both the whole-tree poller (statusd) and the instant file-watcher (watch) maintain, so activity anywhere is captured with no per-call plumbing. -n sets the backlog size, --no-follow prints the backlog and exits, --kind filters to one event kind, --repo filters by path substring, and --json emits NDJSON (one event per line) for tooling.",
        ],
    },
    Doc {
        verb: "ztail",
        summary: "alias of git zevents",
        synopsis: "git ztail [-n <count>] [--kind commit|stage|status|reconcile] [--repo <substr>] [--json] [--no-follow]",
        desc: &["An alias for `git zevents` \\(em the single live feed of commits, reconciles, and status-changes across the whole indexed tree. See `git help zevents` for the full description."],
    },
    Doc {
        verb: "zintercept",
        summary: "AOP hooks that run advice around matching git commands",
        synopsis: "git zintercept before|after|around <pattern> -- <cmd> | list | remove <id> | clear",
        desc: &[
            "Registers aspect-oriented advice that fires around git commands, ported from zshrs. A before hook runs a shell command before a matching git command; an after hook runs after it (with the command's exit status and timing available); an around hook replaces it, and runs the original itself when it chooses. <pattern> matches the git subcommand \\(em `commit`, `push`, a glob like `commit *`, or `*` / `all` for everything.",
            "The advice <cmd> is a shell command run via `sh -c`. It sees the intercepted command through the environment: INTERCEPT_NAME (the subcommand), INTERCEPT_ARGS, and INTERCEPT_CMD (the full `git ...` line); after-advice also gets INTERCEPT_STATUS, INTERCEPT_MS, and INTERCEPT_US. An around advice runs \"$INTERCEPT_CMD\" to proceed with the original (that child is not re-intercepted).",
            "Registrations persist to $ZVCS_HOME/intercepts.tsv and load at dispatch; when none are registered the cost on every git command is a single stat. `list` shows all with their ids, `remove <id>` deletes one, and `clear` removes them all.",
        ],
    },
    Doc {
        verb: "zcommands",
        summary: "live feed of every git command run across the fleet",
        synopsis: "git zcommands [-n <count>] [--repo <substr>] [--json] [--no-follow] [--off] [--clear]",
        desc: &[
            "Streams every `git` command run anywhere on the machine, in real time. Because the zvcs `git` binary shadows real git on PATH, every invocation flows through one dispatcher; when logging is on, each records a line \\(em time, pid, working directory, and the command \\(em to its own log at $ZVCS_HOME/commands.log (separate from the daemon log). This verb prints a backlog and then follows the log live, so commands from every repo and every concurrent shell scroll past as they run.",
            "Running it turns logging on (a marker file, so the cost on every other git command is a single stat when it is off) and notes where the log lives. -n sets the backlog size, --no-follow prints the backlog and exits, --repo filters to commands whose directory contains a substring, and --json emits NDJSON for tooling. --off stops logging; --clear truncates the log. The live viewers (zcommands, zevents, ztail, ztop) are excluded so watching does not feed itself.",
        ],
    },
    Doc {
        verb: "zaudit",
        summary: "queryable audit trail over the fleet command log",
        synopsis: "git zaudit [--agent <ppid>] [--repo <substr>] [--cmd <substr>] [--mutating] [--summary] [-n <count>] [--json]",
        desc: &[
            "The historical, accountable side of the same command log `zcommands` feeds \\(em $ZVCS_HOME/commands.log. Where zcommands follows the live stream, zaudit queries the record: it answers \"which agent ran which command against which repo, and when\" across a fleet of concurrent agents. Each logged line carries the time, the command's pid, its parent pid (the agent that launched it), the working directory, and the git argv.",
            "Filters compose: --agent <ppid> restricts to one agent, --repo <substr> to repos whose directory contains a substring, --cmd <substr> to commands whose argv contains a substring, and --mutating to state-changing subcommands only (commit, push, reset, rebase, merge, checkout, and the mutating z-verbs). -n limits how many matching records print (newest last); --json emits NDJSON.",
            "--summary replaces the record list with tallies: how many commands each agent ran, and how many of each subcommand, ranked by count \\(em an at-a-glance accountability view of who did what across the tree.",
        ],
    },
    Doc {
        verb: "zscan",
        summary: "parallel secret scan of tracked content across the fleet",
        synopsis: "git zscan [selectors]",
        desc: &[
            "Scans the tracked file content of every selected indexed repo, in parallel over the shared worker pool, for common credential patterns \\(em AWS access keys, PEM private keys, GitHub/Slack/Google tokens, JWTs, and high-entropy `key = \"...\"` assignments \\(em using the same native, fork-free content walk `zgrep` does. Each hit prints as path:line:pattern:snippet.",
            "Binary files are skipped, output is capped per repo so one flooded tree cannot drown the rest, and the whole run exits non-zero when anything is found \\(em so `git zscan` drops straight into a pre-push hook or CI gate. [selectors] narrows the set the same way every fleet verb does.",
        ],
    },
    Doc {
        verb: "zsigs",
        summary: "fleet commit-signature check (verify-sigs)",
        synopsis: "git zsigs [selectors] [-n <count>]",
        desc: &[
            "Checks commit signatures across the fleet and flags any that are not a good signature \\(em unsigned (N), bad (B), or unverifiable (E/X/Y/R). By default it checks each selected repo's HEAD; -n walks the top N first-parent commits. Each offender prints as `<code> <repo> <sha> <subject>` (git's `%G?` codes), and the run exits non-zero if any are found \\(em so it gates a push or CI under an all-commits-must-be-signed policy.",
            "Verification is native where it can be and delegated where it must be: the signature and its signed payload are reconstructed from the commit object in-process (the `gpgsig` header removed and de-folded exactly as git does), then handed to gpg \\(em the same tool git shells to; there is no in-process crypto in git either. A commit with no signature is N without running anything; a present signature that gpg cannot check (no public key, gpg absent) is E, never a fabricated good. SSH-format signatures report E in this pass (verifying them needs an allowed-signers file). The same machinery backs the `%G?` and `%GK` pretty-format placeholders in `git log`.",
        ],
    },
    Doc {
        verb: "zreview",
        summary: "aggregate the pending uncommitted change across the fleet",
        synopsis: "git zreview [selectors]",
        desc: &[
            "The read-side companion to `zcommitall`: for every selected repo with uncommitted work, it prints the repo's `git status --short` block grouped under its path, plus the summary line of its diffstat against HEAD, so you can review everything about to be committed across a whole tree of submodules on one screen.",
            "Clean repos are omitted; a trailing line reports how many repos have pending change and how many total entries. The status probe runs through this binary over the shared worker pool, so it is fleet-parallel and reads consistently with every other z-verb.",
        ],
    },
    Doc {
        verb: "zremote",
        summary: "fleet-wide remote-URL rewrite",
        synopsis: "git zremote set <old> <new> [selectors] [-n|--dry-run]",
        desc: &[
            "Rewrites remote URLs across the fleet in one command: in every selected repo, any remote whose URL contains the substring <old> is rewritten with <old> replaced by <new>. This is the one-shot answer to moving an org, changing a host, or switching ssh\\(<->https across a tree of submodules.",
            "Each change prints as `set <repo>::<remote>  <from> → <to>`; -n / --dry-run previews every change as `would set` without touching anything. Writes go through this binary's own `remote set-url` porcelain over the shared worker pool; the run exits non-zero if any set-url fails.",
        ],
    },
    Doc {
        verb: "zrollback",
        summary: "fleet-wide undo of the last mutating operation",
        synopsis: "git zrollback [selectors] [--steps <n>] [--apply] [--force]",
        desc: &[
            "The multi-repo evolution of `zundo`: across every selected repo it resolves `HEAD@{n}` from the reflog (n = --steps, default 1) and rewinds to it with `reset --hard`, undoing the last commit / merge / rebase / reset. The reset is itself reflogged, so a rollback is undoable.",
            "It is **dry-run by default** \\(em with no --apply it prints what each repo *would* do (`<current> → <target>`) and changes nothing. --apply executes the rollback on the repos that pass the guards.",
            "The guards refuse to lose work: a repo is skipped, not rolled back, when its worktree is dirty, when it is mid-operation (merge/rebase/cherry-pick/revert), or when rolling back would diverge from its remote \\(em the discarded commits are already pushed (sync is up-to-date, behind, or diverged). Rollback is allowed by default only when the commits are local-only (ahead or no-upstream). --force overrides every guard.",
        ],
    },
    Doc {
        verb: "zsched",
        summary: "daemon-hosted scheduled fleet commands (a built-in cron)",
        synopsis: "git zsched add <duration> -- <cmd> | list | rm <id> | clear | run <id>",
        desc: &[
            "Schedules a command to run on an interval, hosted by the daemon \\(em a built-in cron for the whole tree. `git zsched add 5m -- git zpull --dirty` fast-forwards every dirty repo every five minutes; durations are 30s / 5m / 1h / 1h30m style. `list` shows every schedule with its id and interval, `rm <id>` removes one, `clear` removes them all, and `run <id>` fires one now (synchronously) for testing.",
            "Schedules persist to $ZVCS_HOME/schedule.tsv as `id\\tinterval\\tcommand`. The CLI owns every write to that file; the daemon's scheduler thread only reads it (re-reading each tick, so add/rm take effect within one tick with no reload) and tracks each schedule's last-fire time in memory \\(em so the CLI and daemon never contend on the file. A schedule fires one full interval after the daemon first sees it, not on daemon start.",
        ],
    },
    Doc {
        verb: "zpin",
        summary: "freeze a repo from daemon autonomy",
        synopsis: "git zpin [<path>...|list] [--json]",
        desc: &[
            "Pins one or more repositories so the daemon's autonomy leaves their pointer alone \\(em a pinned repo is skipped by autobump and reconcile, so its gitlink and HEAD will not move on their own until it is unpinned. Detached-HEAD attach still runs, because re-attaching a detached HEAD heals it without moving the pointer.",
            "With no path it pins the repository at the current directory; with paths it pins each named repository; `git zpin list` prints every pinned repo. The flag lives in the shared index (`repos.pinned`), so it survives daemon restarts and is visible to every session. Unfreeze with `git zunpin`.",
        ],
    },
    Doc {
        verb: "zunpin",
        summary: "unfreeze repos pinned from autonomy",
        synopsis: "git zunpin [<path>...|--all]",
        desc: &[
            "Clears the pin set by `git zpin`, so the daemon resumes autobump and reconcile for the repository. With no path it unpins the repo at the current directory; with paths it unpins each named one; --all unpins every pinned repository at once.",
        ],
    },
    Doc {
        verb: "zbroadcast",
        summary: "post and read inter-agent messages",
        synopsis: "git zbroadcast [--to <session>] [<msg>...]",
        desc: &[
            "Sends a message to the other agent sessions working the tree, over the shared index. With a message it broadcasts to every session, or to one with --to <session>; with no arguments it prints this session's unread messages and marks them read.",
            "Read state is tracked per session, so a broadcast is delivered once to each recipient and a sender never sees its own message. It is a pull inbox \\(em messages are read when you run the verb, adding no cost to any other command. Session identity comes from ZVCS_SESSION.",
        ],
    },
    Doc {
        verb: "zhandoff",
        summary: "hand a repo's claim to another agent session",
        synopsis: "git zhandoff <repo-path> <session>",
        desc: &[
            "Reassigns the advisory claim (lease) on a repository from the current holder to another session, and drops that session a message noting the handoff. Fails if the repository is not currently claimed.",
            "Claims are the session-attributed \"I'm working this repo\" signal set by `git zclaim`; zhandoff transfers one without the receiver having to race for it, so work can be passed cleanly between agents.",
        ],
    },
    Doc {
        verb: "zon",
        summary: "run a command on a semantic feed event",
        synopsis: "git zon [--kind commit|stage|status|reconcile] [--repo <substr>] -- <cmd> | git zon list | git zon rm <id>",
        desc: &[
            "Registers a rule that runs a command when a matching event fires on the live feed. Where `git ztrigger` reacts to raw filesystem events under a directory, zon reacts to typed events \\(em commit, stage, status, reconcile \\(em from the same feed `git zevents` shows, optionally filtered to repos whose path contains a substring.",
            "The command after -- runs via the shell with ZVCS_EVENT, ZVCS_REPO, ZVCS_DETAIL, and ZVCS_SHA set for the triggering event. `git zon list` shows every subscription; `git zon rm <id>` removes one. The daemon watches the feed and runs the matches. Caveat (shared with ztrigger): a command that itself commits or stages can match its own subscription and re-fire \\(em scope it with --kind/--repo.",
        ],
    },
    Doc {
        verb: "zsince",
        summary: "what happened across the tree since a time",
        synopsis: "git zsince <duration|snapshot> [--kind K] [--repo R] [--json]",
        desc: &[
            "Prints every feed event \\(em commits, stages, status changes, reconciles \\(em recorded across the whole indexed tree since a point in time. A duration (90s, 45m, 2d, 1h30m, or a bare number of seconds) counts back from now; any other token is treated as a snapshot name and its creation time is the baseline.",
            "This is the scoped \"what did the fleet do since X\" delta that `git zlog`'s full timeline does not give. --kind and --repo narrow the result to one event kind or to repos matching a substring.",
        ],
    },
    Doc {
        verb: "zcontend",
        summary: "live agent-vs-agent contention across the tree",
        synopsis: "git zcontend [--json]",
        desc: &[
            "Shows who is stepping on whom: the session-attributed claims (advisory leases), the per-repo job backlog (how many jobs are queued or running behind each repository in the daemon's fair queue), and the intersection \\(em repositories that are both claimed and have jobs stacked up, i.e. actively contested.",
            "All three are read from the shared index; nothing is mutated. It is the multi-agent observability view git has no notion of, because git only ever sees one repository and one process.",
        ],
    },
    Doc {
        verb: "zwaitfor",
        summary: "block until a tree-wide state holds",
        synopsis: "git zwaitfor <clean|idle|synced|<repo> <sha>> [--timeout <secs>]",
        desc: &[
            "Blocks until a condition over the whole tree becomes true, then exits 0 (or 1 on timeout) \\(em a cross-repo barrier on state, where `git zbarrier` and `git zwait` are job-scoped. clean waits until every indexed repo is clean; idle until no daemon jobs are queued or running; synced until every repo is up-to-date with its upstream; `<substr> <sha>` until the repo whose path contains the substring is at the given commit (prefix).",
            "Conditions are read from the daemon's cached status, so the daemon must be maintaining status for the wait to observe changes. --timeout bounds the wait (default 60 seconds).",
        ],
    },
    Doc {
        verb: "zgraph",
        summary: "fleet topology of duplicate checkouts",
        synopsis: "git zgraph [--json]",
        desc: &[
            "Maps the cross-repository relationship git has no notion of: which local checkouts are the same upstream repository \\(em a dup group, the same origin URL with more than one working tree on the machine. git can never show this, because git only ever sees one repository.",
            "Reads every indexed repository's origin (on demand, so the per-repo config read is acceptable) and lists each dup group with its checkouts, then a summary of repos, distinct origins, dup groups, and repos with no origin.",
        ],
    },
    Doc {
        verb: "zrewind",
        summary: "restore the whole tree to a wall-clock time",
        synopsis: "git zrewind <duration> [--dry-run]",
        desc: &[
            "Rewinds the tree \\(em the repository at the current directory and every nested submodule \\(em to the state it was in a given duration ago (2h, 30m, 1d). For each repository it finds the HEAD the reflog shows it had at that time and resets hard to it, reusing the faithful porcelain reset (itself reflogged, so the rewind is undoable). Where `git zsnapshot` is a manual named restore point, this is any timestamp with no prior setup.",
            "A dirty repository is refused so uncommitted work is never clobbered, and a repository whose reflog does not reach that far back is reported and skipped (the reflog's default 90-day expiry bounds how far back it can go). --dry-run shows exactly what would move without changing anything.",
        ],
    },
    Doc {
        verb: "zguard",
        summary: "declarative fleet-wide command policy (refuse or warn)",
        synopsis: "git zguard deny|warn <pattern> [--when <predicate>] [-m <msg>] | list | rm <id> | clear | test <cmd>...",
        desc: &[
            "Registers rules that refuse or warn on a git command before it runs \\(em a pattern-based command policy applied at the shadow binary, so it covers every invocation through it. The veto evolution of `zintercept`'s around-advice, promoted to a first-class policy layer no VCS has natively.",
            "A rule is an action (deny or warn), a glob pattern matched against the whole command line (`*` and `?`, same matcher as `zintercept`), an optional repo-state predicate, and an optional message. `git zguard deny 'push*--force*'` refuses a force-push; `git zguard warn 'rm*-rf*'` warns but allows.",
            "The `--when <predicate>` conditions a rule on the current repository: detached (HEAD is detached), dirty (uncommitted changes), protected (HEAD is main or master), unsigned (a commit with no -S/--gpg-sign and commit.gpgsign unset). So `git zguard deny 'commit*' --when detached` blocks commits on a detached HEAD, and `--when unsigned` requires signed commits.",
            "list shows the rules; rm <id> removes one; clear removes all; test <cmd>... reports the verdict (DENY/WARN/allow) for a command without running it. Rules are stored in $ZVCS_HOME/guards.tsv (machine-wide); the dispatch hot path is a single stat when no rule is set. The zguard/zpolicy verbs are exempt from their own rules, so a bad rule can never lock you out.",
        ],
    },
    Doc {
        verb: "zpolicy",
        summary: "alias of git zguard (command policy)",
        synopsis: "git zpolicy deny|warn <pattern> [--when <predicate>] [-m <msg>] | list | rm <id> | clear | test <cmd>...",
        desc: &["An alias for `git zguard` \\(em declarative fleet-wide command policy. See `git help zguard` for the full description."],
    },
    Doc {
        verb: "zppid",
        summary: "per-process commit tally, attributed to the durable process",
        synopsis: "git zppid",
        desc: &[
            "Reports which process is responsible for each recorded commit, found by walking up past the throwaway per-command shells to the durable agent, program, or login shell that drove them.",
            "Each row carries the pid, its command and working directory, whether it is still alive, and how many commits it landed.",
        ],
    },
    Doc {
        verb: "zprocs",
        summary: "per-process breakdown of mutating commands",
        synopsis: "git zprocs",
        desc: &["Reports how many of each mutating verb \\(em commit, push, add, merge, rebase and the rest \\(em every recorded process has run, attributed the same way as `git zppid`."],
    },
    Doc {
        verb: "zprecache",
        summary: "precompute the log caches for recent commits",
        synopsis: "git zprecache [-n <commits>] [-q]",
        desc: &[
            "Fills the ledger's log caches \\(em commit abbreviations and the per-file line tallies that --stat, --numstat, --shortstat and --name-status need \\(em for the newest commits, so those formats read the ledger instead of the object store.",
            "Everything it stores is a pure function of immutable objects, so an entry never expires. -n sets how many commits to walk (default 200); -q suppresses the count.",
            "The daemon does this on its own whenever a watched repository's refs move (zvcs.precache, on by default); this verb is the same pass on demand, for a fresh clone or a fetch that landed while the daemon was down.",
        ],
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

/// Self-contained CSS for the HTML reference — inlined so the page needs no
/// assets, works offline, and adapts to light/dark. Shared with
/// [`crate::superset::htmldoc`] so the installed doc set and the published
/// reference render alike.
pub(crate) const HTML_STYLE: &str = "<style>\n\
:root{--bg:#0d1117;--fg:#c9d1d9;--dim:#8b949e;--accent:#58a6ff;--code:#1f2430;--line:#21262d}\n\
@media(prefers-color-scheme:light){:root{--bg:#fff;--fg:#1f2328;--dim:#57606a;--accent:#0969da;--code:#f2f4f8;--line:#d0d7de}}\n\
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.6 -apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif;max-width:60rem;margin:0 auto;padding:2rem 1.25rem}\n\
h1{font-size:1.9rem;margin:0 0 .25rem}.sub{color:var(--dim);margin:0 0 1.5rem}\n\
code,pre{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}\n\
code{background:var(--code);padding:.1em .35em;border-radius:4px;font-size:.9em}\n\
.toc{display:flex;flex-wrap:wrap;gap:.4rem .6rem;padding:1rem;background:var(--code);border-radius:8px;margin-bottom:2rem}\n\
.toc a{text-decoration:none;color:var(--accent)}.toc a code{background:transparent;padding:0}\n\
section{border-top:1px solid var(--line);padding-top:1.25rem;margin-top:1.25rem}\n\
h2{font-size:1.15rem;margin:0 0 .5rem;scroll-margin-top:1rem}h2 code{background:transparent;padding:0;color:var(--accent)}\n\
.summary{color:var(--dim);font-weight:400;font-size:.95rem}\n\
pre.synopsis{background:var(--code);padding:.6rem .8rem;border-radius:6px;overflow-x:auto;font-size:.9em;margin:.5rem 0 1rem}\n\
p{margin:.6rem 0}a{color:var(--accent)}\n\
</style>\n";

/// Render every verb's [`Doc`] into one self-contained HTML reference page, from
/// the same `DOCS` table as the man pages — so it can't drift. `git zverbs --html`
/// prints this; `docs/reference.html` is a committed snapshot for the docs site.
pub fn html_reference() -> String {
    let mut s = String::new();
    s.push_str("<!doctype html>\n<html lang=\"en\"><head>\n<meta charset=\"utf-8\">\n");
    s.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    s.push_str("<title>zvcs — extension verb reference</title>\n");
    s.push_str(HTML_STYLE);
    s.push_str("</head><body>\n");
    s.push_str("<h1>zvcs extension verbs</h1>\n");
    s.push_str(&format!(
        "<p class=\"sub\">{} superset (<code>z*</code>) verbs — the coordination layer stock git has no equivalent for. Generated from the same source as the man pages (<code>git help &lt;verb&gt;</code>).</p>\n",
        DOCS.len()
    ));
    s.push_str("<nav class=\"toc\">\n");
    for d in DOCS {
        s.push_str(&format!("<a href=\"#{v}\"><code>{v}</code></a>\n", v = d.verb));
    }
    s.push_str("</nav>\n");
    for d in DOCS {
        s.push_str(&format!("<section id=\"{v}\">\n", v = d.verb));
        s.push_str(&format!(
            "<h2><code>git {v}</code> <span class=\"summary\">— {sum}</span></h2>\n",
            v = d.verb,
            sum = html_esc(d.summary)
        ));
        s.push_str(&format!("<pre class=\"synopsis\">{}</pre>\n", html_esc(d.synopsis)));
        for para in d.desc {
            s.push_str(&format!("<p>{}</p>\n", html_inline(para)));
        }
        s.push_str("</section>\n");
    }
    s.push_str("</body></html>\n");
    s
}

pub(crate) fn html_esc(t: &str) -> String {
    t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// HTML-escape a description paragraph, turn the roff em-dash `\(em` into `—`, and
/// `` `code` `` spans into `<code>`.
pub(crate) fn html_inline(t: &str) -> String {
    let esc = html_esc(t).replace("\\(em", "\u{2014}");
    let mut out = String::new();
    let mut in_code = false;
    for c in esc.chars() {
        if c == '`' {
            out.push_str(if in_code { "</code>" } else { "<code>" });
            in_code = !in_code;
        } else {
            out.push(c);
        }
    }
    if in_code {
        out.push_str("</code>");
    }
    out
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
