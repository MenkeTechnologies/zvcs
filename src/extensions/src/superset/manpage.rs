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
        summary: "list the working directory",
        synopsis: "git zls [<ls-args>...]",
        desc: &["Lists the working directory by delegating to the system ls, so every ls flag (-l, -a, and so on) and its output work as-is. Exits with ls's own status. A shell-like convenience for the zrepl console."],
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
