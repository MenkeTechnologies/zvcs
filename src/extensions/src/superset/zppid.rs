//! `git zppid` — per-process commit productivity, plus the commit-tracking hook
//! the dispatcher fires after every commit-producing verb.
//!
//! The fleet runs many processes that drive git concurrently — one per agent /
//! login. The naive identity, `getppid()`, is useless: a `git commit` is run by a
//! throwaway shell (an agent spawns a fresh `zsh -c …` per command, or a script
//! subshells), so the parent pid is different on every commit. Keying on it yields
//! a flood of one-commit rows, never a stable "N agents" view.
//!
//! So instead of the immediate parent we find the **responsible process**: walk up
//! the parent chain, skip transient wrapper shells (a shell invoked with `-c`, which
//! runs one command and dies), and stop at the first durable process — a real
//! program (an agent, editor, daemon) or an interactive login shell. That pid is
//! stable across every commit one agent makes, so N concurrent agents map to N rows.
//! Each row also carries that process's command name and cwd, so the pid is legible.
//!
//! Attribution happens at command time: [`note_commit`] is called by
//! [`crate::dispatch::run`] right after any [commit-producing verb](COMMIT_VERBS)
//! returns, and credits the responsible process only when HEAD actually advanced —
//! a no-op `commit` (nothing staged) or a rejected merge counts nothing.

use anyhow::Result;
use std::path::PathBuf;
use std::process::ExitCode;

/// Verbs that can add a commit to the current branch. A successful invocation of
/// one of these that moves HEAD to a new tip is credited to the responsible process.
/// `rebase`/`merge` may move HEAD without authoring the commit(s); that is counted
/// as one landed operation, not one per replayed commit — see the module docs.
const COMMIT_VERBS: &[&str] = &["commit", "commit-tree", "merge", "cherry-pick", "revert", "am", "rebase"];

/// Whether `sub` is a commit-producing verb worth probing HEAD around.
pub fn is_commit_verb(sub: &str) -> bool {
    COMMIT_VERBS.contains(&sub)
}

/// The current repo's peeled HEAD commit id, or `None` outside a repo / on an
/// unborn branch. The cheap before/after probe [`note_commit`] compares. Any error
/// (not a repo, detached read failure) collapses to `None` — a `None → Some`
/// transition is the first commit on an unborn branch and still counts.
pub fn head_commit() -> Option<gix::ObjectId> {
    let repo = gix::discover(".").ok()?;
    repo.head().ok()?.try_peel_to_id().ok().flatten().map(|id| id.detach())
}

/// Whether `argv0` names a shell (by basename, ignoring a login shell's leading `-`).
fn is_shell(argv0: &str) -> bool {
    let base = argv0.rsplit('/').next().unwrap_or(argv0).trim_start_matches('-');
    matches!(base, "sh" | "bash" | "zsh" | "dash" | "fish" | "ksh" | "tcsh" | "csh")
}

/// A short command name from an argv0 path: the basename, minus a login shell's `-`.
fn cmd_name(argv0: &str) -> String {
    argv0.rsplit('/').next().unwrap_or(argv0).trim_start_matches('-').to_string()
}

/// `(ppid, argv0, is_wrapper)` for `pid`, read straight from the kernel — no
/// `ps`, no fork. `is_wrapper` is true for a shell invoked with `-c`, a
/// transient per-command wrapper to climb past. `None` when the pid is gone.
///
/// Forking here was the single most expensive thing zvcs did: every mutating
/// verb spawned `ps` per ancestor hop and then `lsof` for the cwd, and `lsof`
/// alone measures ~52ms on macOS — a flat ~57ms tax on `add`/`commit`/`push`
/// regardless of repository size, which made `git add` 6x slower than stock git
/// for reasons that had nothing to do with git.
fn proc_row(pid: i64) -> Option<(i64, String, bool)> {
    let (ppid, argv) = proc_parent_and_argv(pid)?;
    let argv0 = argv.first()?.clone();
    let is_wrapper = is_shell(&argv0) && argv.iter().any(|a| a == "-c");
    Some((ppid, argv0, is_wrapper))
}

/// Linux: just the parent pid, no argv. `/proc/<pid>/stat` field 4, scanned past
/// the comm field's closing `)` because comm may itself contain spaces and
/// parentheses.
///
/// Split out from [`proc_parent_and_argv`] because the ancestry walk — which runs
/// on every daemon-backed lock acquisition — never looks at argv, and reading it
/// costs a second file (a 4 KiB `sysctl` buffer on macOS) per hop.
#[cfg(target_os = "linux")]
fn proc_ppid(pid: i64) -> Option<i64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Parent pid and argv of `pid` through the platform's process-info syscall.
#[cfg(target_os = "linux")]
fn proc_parent_and_argv(pid: i64) -> Option<(i64, Vec<String>)> {
    let ppid = proc_ppid(pid)?;
    // `/proc/<pid>/cmdline` is NUL-separated argv.
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let argv: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    Some((ppid, argv))
}

/// macOS/BSD: `proc_pidinfo` for the parent, `KERN_PROCARGS2` for argv. Both are
/// plain syscalls; neither allocates a process.
#[cfg(not(target_os = "linux"))]
fn proc_parent_and_argv(pid: i64) -> Option<(i64, Vec<String>)> {
    Some((proc_ppid(pid)?, proc_argv(pid)))
}

/// macOS/BSD: just the parent pid, no argv.
#[cfg(not(target_os = "linux"))]
fn proc_ppid(pid: i64) -> Option<i64> {
    // `proc_pidinfo(PROC_PIDTBSDINFO)` carries the parent pid at a fixed offset
    // in `proc_bsdinfo`. Read as raw bytes rather than transcribing the whole
    // struct — libc does not export it on this target, and the offset is part of
    // the stable ABI `lsof` itself relies on.
    //
    // `pbi_ppid` is the FIFTH u32, at byte 16: the struct opens
    // `pbi_flags`(0), `pbi_status`(4), `pbi_xstatus`(8), `pbi_pid`(12),
    // `pbi_ppid`(16). Byte 12 — which this read for a while — is the process's
    // OWN pid, so every ancestry walk terminated instantly on `ppid == pid` and
    // silently reported no ancestors at all.
    const PROC_PIDTBSDINFO: libc::c_int = 3;
    const PBI_PPID_OFFSET: usize = 16;
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: libc::c_int,
            flavor: libc::c_int,
            arg: u64,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }
    let mut buf = [0u8; 256];
    let rc = unsafe {
        proc_pidinfo(
            pid as libc::c_int,
            PROC_PIDTBSDINFO,
            0,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len() as libc::c_int,
        )
    };
    if rc <= 0 {
        return None;
    }
    Some(u32::from_ne_bytes([
        buf[PBI_PPID_OFFSET],
        buf[PBI_PPID_OFFSET + 1],
        buf[PBI_PPID_OFFSET + 2],
        buf[PBI_PPID_OFFSET + 3],
    ]) as i64)
}

/// argv of `pid` via `sysctl(KERN_PROCARGS2)`, whose buffer is
/// `argc`, the executable path, then NUL-separated argv (with padding NULs
/// between the path and argv[0] that have to be skipped).
#[cfg(not(target_os = "linux"))]
fn proc_argv(pid: i64) -> Vec<String> {
    const KERN_PROCARGS2: libc::c_int = 49;
    unsafe {
        let mut size: usize = 4096;
        let mut buf = vec![0u8; size];
        let mut mib = [libc::CTL_KERN, KERN_PROCARGS2, pid as libc::c_int];
        if libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return Vec::new();
        }
        buf.truncate(size);
        if buf.len() < 4 {
            return Vec::new();
        }
        let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]).max(0) as usize;
        let mut parts = buf[4..].split(|b| *b == 0);
        // The first entry is the executable path; the padding NULs after it are
        // empty splits, so argv starts at the first non-empty entry beyond it.
        let _exec_path = parts.next();
        parts
            .filter(|p| !p.is_empty())
            .take(argc)
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect()
    }
}

/// A process's current working directory, without spawning anything: Linux reads
/// the `/proc/<pid>/cwd` symlink, macOS asks `proc_pidinfo` for the vnode path
/// info — the same call `lsof` itself makes, minus the process.
fn proc_cwd(pid: i64) -> String {
    #[cfg(target_os = "linux")]
    {
        return std::fs::read_link(format!("/proc/{pid}/cwd"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    #[cfg(not(target_os = "linux"))]
    {
        proc_cwd_darwin(pid).unwrap_or_default()
    }
}

#[cfg(not(target_os = "linux"))]
fn proc_cwd_darwin(pid: i64) -> Option<String> {
    // `PROC_PIDVNODEPATHINFO` returns two `vnode_info_path` structs (cwd first,
    // then the root dir); each ends with a `MAXPATHLEN` C string. Only the cwd
    // path is read here, so the struct is modelled as raw bytes rather than
    // transcribed field by field.
    const PROC_PIDVNODEPATHINFO: libc::c_int = 9;
    const MAXPATHLEN: usize = 1024;
    // vnode_info (152 bytes) + path buffer, twice.
    const VNODE_INFO_PATH: usize = 152 + MAXPATHLEN;
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: libc::c_int,
            flavor: libc::c_int,
            arg: u64,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }
    let mut buf = vec![0u8; VNODE_INFO_PATH * 2];
    let rc = unsafe {
        proc_pidinfo(
            pid as libc::c_int,
            PROC_PIDVNODEPATHINFO,
            0,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len() as libc::c_int,
        )
    };
    if rc <= 0 {
        return None;
    }
    let path = &buf[152..VNODE_INFO_PATH];
    let end = path.iter().position(|b| *b == 0).unwrap_or(path.len());
    let text = String::from_utf8_lossy(&path[..end]).into_owned();
    (!text.is_empty()).then_some(text)
}

/// The durable process responsible for this git command: `(pid, cmd, cwd)`. Walks
/// up from `getppid()`, skipping transient `-c` wrapper shells, and stops at the
/// first durable process (a real program or an interactive shell). Bounded so a
/// pid-reuse cycle can never spin.
fn responsible_process() -> (i64, String, String) {
    let mut pid = std::os::unix::process::parent_id() as i64;
    let (mut chosen, mut cmd) = (pid, String::new());
    for _ in 0..32 {
        let Some((ppid, argv0, is_wrapper)) = proc_row(pid) else { break };
        if is_wrapper && ppid > 1 {
            pid = ppid; // transient `sh -c` wrapper → keep climbing
            continue;
        }
        chosen = pid;
        cmd = cmd_name(&argv0);
        break;
    }
    (chosen, cmd, proc_cwd(chosen))
}

/// The resolved responsible process for an attributed command: its durable pid,
/// its session key, and its command name / cwd for display.
pub struct Responsible {
    pid: i64,
    session: String,
    cmd: String,
    cwd: String,
}

/// Start resolving the responsible process on a BACKGROUND thread, so the
/// process-info syscalls overlap the git command instead of being paid after it.
///
/// The caller kicks this off before running the verb and collects it afterwards;
/// by then the answer is almost always already there, and the ledger write is
/// all that remains on the critical path.
pub fn spawn_resolve() -> std::thread::JoinHandle<Responsible> {
    std::thread::spawn(resolve_responsible)
}

/// Resolve the durable responsible process and its session key — an explicit
/// `ZVCS_SESSION` when set, else `pid-<responsible-pid>` so one agent is one row.
fn resolve_responsible() -> Responsible {
    let (pid, cmd, cwd) = responsible_process();
    let session = std::env::var("ZVCS_SESSION")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("pid-{pid}"));
    Responsible { pid, session, cmd, cwd }
}

/// Mutating verbs whose invocations are tallied per process (`git zprocs`). Chosen
/// to be state-changing but relatively infrequent, so the durable-process walk stays
/// off the read hot path (`rev-parse`/`status`/`log` never trigger it).
const MUTATING_VERBS: &[&str] = &[
    "commit", "push", "add", "merge", "rebase", "reset",
    "checkout", "cherry-pick", "revert", "tag", "stash", "fetch", "pull",
];

/// Whether `sub` is a tracked mutating verb.
pub fn is_mutating_verb(sub: &str) -> bool {
    MUTATING_VERBS.contains(&sub)
}

/// Record one mutating-verb invocation for the responsible process. Resolves the
/// durable process once and shares it across both tables. For `commit` the effective
/// commit tally (`ppids.commits`) is bumped only when HEAD actually advanced from
/// `before_head` — a no-op commit adds nothing — and only then is the verb counted.
/// Every other mutating verb registers the process and bumps its per-verb count.
/// Best-effort: swallows every error so it can never fail or slow the command.
pub fn note_mutating(
    sub: &str,
    before_head: Option<gix::ObjectId>,
    resolver: Option<std::thread::JoinHandle<Responsible>>,
) {
    // The identity was resolved on a background thread while the command ran;
    // fall back to resolving inline only if no thread was started.
    let who = match resolver {
        Some(handle) => match handle.join() {
            Ok(who) => who,
            Err(_) => return,
        },
        None => resolve_responsible(),
    };
    let Ok(conn) = crate::db::open_rw() else { return };
    if is_commit_verb(sub) {
        let after = head_commit();
        if after.is_none() || after == before_head {
            return; // no new commit landed
        }
        let _ = crate::db::ppid_record_commit(&conn, &who.session, who.pid, &who.cmd, &who.cwd);
        let _ = crate::db::proc_verb_record(&conn, &who.session, "commit");
    } else {
        let _ = crate::db::ppid_touch(&conn, &who.session, who.pid, &who.cmd, &who.cwd);
        let _ = crate::db::proc_verb_record(&conn, &who.session, sub);
    }
}

/// Whether `pid` is still a live process. `kill(pid, 0)` succeeds for a live,
/// signalable process; `EPERM` means it exists but is owned by another uid (still
/// alive); `ESRCH` (and anything else) means gone. Uses `last_os_error` rather than
/// a raw `errno` symbol so it builds identically on macOS and Linux. Shared with the
/// dashboard's process tile.
pub fn is_alive(pid: i64) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// How far up the process tree the ancestry probes walk before giving up. Deep
/// enough for `clone → submodule update → submodule update → apply` plus the
/// shells in between, shallow enough that a cycle in a corrupt process table
/// cannot spin.
const MAX_ANCESTOR_HOPS: usize = 64;

/// The current process's ancestor pids, nearest first. Stops at pid 1 / an
/// unreadable process, so the list is only ever as long as the platform will
/// actually report.
fn ancestry() -> Vec<i64> {
    let mut out = Vec::new();
    let mut pid = std::process::id() as i64;
    for _ in 0..MAX_ANCESTOR_HOPS {
        let Some(ppid) = proc_ppid(pid) else {
            break;
        };
        // pid 1 is launchd/init — an ancestor of everything and never informative.
        if ppid <= 1 || ppid == pid {
            break;
        }
        out.push(ppid);
        pid = ppid;
    }
    out
}


/// The on-disk executable a process is running, or `None` if it cannot be read.
///
/// Deliberately NOT argv[0]: argv is what the parent chose to pass and, on this
/// platform, `sysctl(KERN_PROCARGS2)` returns nothing at all for processes other
/// than the caller — so identifying an ancestor by its argv silently identified
/// nothing. The executable path comes from a different syscall that does answer
/// for same-uid processes, and cannot be spoofed by an odd argv[0].
fn proc_exe_path(pid: i64) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        return std::fs::read_link(format!("/proc/{pid}/exe")).ok();
    }
    #[cfg(not(target_os = "linux"))]
    {
        const PROC_PIDPATHINFO_MAXSIZE: usize = 4 * 1024;
        unsafe extern "C" {
            fn proc_pidpath(pid: libc::c_int, buffer: *mut libc::c_void, buffersize: u32) -> libc::c_int;
        }
        let mut buf = vec![0u8; PROC_PIDPATHINFO_MAXSIZE];
        let n = unsafe {
            proc_pidpath(
                pid as libc::c_int,
                buf.as_mut_ptr() as *mut libc::c_void,
                PROC_PIDPATHINFO_MAXSIZE as u32,
            )
        };
        if n <= 0 {
            return None;
        }
        buf.truncate(n as usize);
        Some(PathBuf::from(String::from_utf8_lossy(&buf).into_owned()))
    }
}

/// Whether `pid` is a STRICT ancestor of this process (parent, grandparent, …).
///
/// This is what makes the repo lane reentrant across a process boundary. A `git`
/// child spawned by a `git` parent that already holds the lane must recognize the
/// holder as its own ancestor: the ancestor is blocked waiting on this child, so
/// it cannot release, and treating the lane as free for the duration of the child
/// is exactly the mutual exclusion the ancestor already established.
///
/// The current process is deliberately NOT a match — a second thread of this same
/// process holding the lane must still serialize through the daemon, which is what
/// [`crate::lock`]'s thread-local reentrancy set already governs.
///
/// Pid reuse cannot produce a false positive here: the walk only ever compares
/// against pids that are live ancestors *right now*, resolved from the platform's
/// own process table rather than from anything the holder told us.
pub fn is_ancestor(pid: i64) -> bool {
    pid > 1 && ancestry().contains(&pid)
}

/// Whether this process was spawned (directly or through intermediate shells) by
/// another `git`/`zvcs` process — `Some(true)`/`Some(false)` — or `None` when the
/// platform would not report our own ancestry at all.
///
/// A synchronous child of a `git` parent must never be turned into a background
/// job: the parent is waiting for the child's EFFECT, and a queued job returns
/// exit 0 having done nothing, so the parent reports success on work that never
/// happened. Callers treat `None` the same as `Some(true)` — blocking on a lane is
/// never a wrong answer, whereas queueing can be.
pub fn git_ancestor() -> Option<bool> {
    let chain = ancestry();
    if chain.is_empty() {
        return None; // ancestry unavailable — caller must assume the unsafe case
    }
    let self_exe = crate::hosted::git_exe().ok();
    let mut any_readable = false;
    let mut found = false;
    for pid in chain {
        let Some(exe) = proc_exe_path(pid) else {
            continue; // a process we may not inspect; keep walking
        };
        any_readable = true;
        if Some(&exe) == self_exe.as_ref() {
            found = true;
            break;
        }
        if let Some(name) = exe.file_name().and_then(|n| n.to_str()) {
            // `git` covers the shadow binary under its install name and stock git
            // alike — a stock-git parent waiting on a zvcs child is the same hazard.
            if name == "git" || name == "zvcs" {
                found = true;
                break;
            }
        }
    }
    // Not one ancestor's executable was readable: we learned nothing, and
    // "learned nothing" must not read as "safe to defer".
    if !any_readable {
        return None;
    }
    Some(found)
}

/// Replace a leading `$HOME` with `~` for compact display.
pub fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    }
}

/// `git zppid [--json]` — list every tracked process, most commits first.
pub fn zppid(args: &[String]) -> Result<ExitCode> {
    let json = args.iter().any(|a| a == "--json");

    let Ok(conn) = crate::db::open_ro() else {
        if json {
            println!("[]");
        } else {
            println!("no processes recorded yet");
        }
        return Ok(ExitCode::SUCCESS);
    };
    let procs = crate::db::list_ppids(&conn)?;
    let now = crate::date::now_seconds();

    if json {
        let arr: Vec<_> = procs
            .iter()
            .map(|p| {
                serde_json::json!({
                    "pid": p.ppid,
                    "cmd": p.cmd,
                    "cwd": p.cwd,
                    "alive": is_alive(p.ppid),
                    "commits": p.commits,
                    "first_seen": p.first_seen,
                    "last_seen": p.last_seen,
                })
            })
            .collect();
        println!("{}", serde_json::json!(arr));
        return Ok(ExitCode::SUCCESS);
    }

    if procs.is_empty() {
        println!("no processes recorded yet");
        return Ok(ExitCode::SUCCESS);
    }

    let total: i64 = procs.iter().map(|p| p.commits).sum();
    let live = procs.iter().filter(|p| is_alive(p.ppid)).count();
    println!("{} process(es), {live} live, {total} commit(s):", procs.len());
    println!("  {:>7} {:>5} {:>7} {:>5}  {:<16} CWD", "PID", "STATE", "COMMITS", "LAST", "CMD");
    for p in &procs {
        let state = if is_alive(p.ppid) { "live" } else { "dead" };
        println!(
            "  {:>7} {:>5} {:>7} {:>5}  {:<16} {}",
            p.ppid,
            state,
            p.commits,
            ago((now - p.last_seen).max(0)),
            trunc(&p.cmd, 16),
            tilde(&p.cwd),
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// `git zprocs [--json]` — per-process breakdown of the mutating commands each
/// process has run (commit, push, add, merge, …), busiest first. Joins the process
/// identity (`ppids`) with the per-verb tally (`proc_verbs`).
pub fn zprocs(args: &[String]) -> Result<ExitCode> {
    use std::collections::HashMap;
    let json = args.iter().any(|a| a == "--json");

    let Ok(conn) = crate::db::open_ro() else {
        if json {
            println!("[]");
        } else {
            println!("no process activity recorded yet");
        }
        return Ok(ExitCode::SUCCESS);
    };
    let procs = crate::db::list_ppids(&conn)?;
    let verbs = crate::db::list_proc_verbs(&conn)?;
    let now = crate::date::now_seconds();

    // session -> [(verb, count)], preserving the count-desc order from the query.
    let mut by_session: HashMap<String, Vec<(String, i64)>> = HashMap::new();
    for v in &verbs {
        by_session.entry(v.session.clone()).or_default().push((v.verb.clone(), v.count));
    }
    // Order processes by their total mutating commands, busiest first.
    #[allow(clippy::type_complexity)] // borrowed PpidRow makes a lifetime-parameterized alias awkward
    let mut rows: Vec<(&crate::db::PpidRow, Vec<(String, i64)>, i64)> = procs
        .iter()
        .filter_map(|p| {
            let vs = by_session.get(&p.session).cloned().unwrap_or_default();
            if vs.is_empty() {
                return None; // process with no recorded mutating verbs yet
            }
            let total: i64 = vs.iter().map(|(_, c)| c).sum();
            Some((p, vs, total))
        })
        .collect();
    rows.sort_by_key(|x| std::cmp::Reverse(x.2));

    if json {
        let arr: Vec<_> = rows
            .iter()
            .map(|(p, vs, total)| {
                let verbs: serde_json::Map<String, serde_json::Value> =
                    vs.iter().map(|(v, c)| (v.clone(), serde_json::json!(c))).collect();
                serde_json::json!({
                    "pid": p.ppid,
                    "cmd": p.cmd,
                    "cwd": p.cwd,
                    "alive": is_alive(p.ppid),
                    "total": total,
                    "verbs": verbs,
                })
            })
            .collect();
        println!("{}", serde_json::json!(arr));
        return Ok(ExitCode::SUCCESS);
    }

    if rows.is_empty() {
        println!("no process activity recorded yet");
        return Ok(ExitCode::SUCCESS);
    }

    let grand: i64 = rows.iter().map(|(_, _, t)| t).sum();
    println!("{} process(es), {grand} mutating command(s):", rows.len());
    println!("  {:>7} {:>5} {:>6} {:>5}  {:<12} COMMANDS", "PID", "STATE", "TOTAL", "LAST", "CMD");
    for (p, vs, total) in &rows {
        let state = if is_alive(p.ppid) { "live" } else { "dead" };
        let breakdown = vs.iter().map(|(v, c)| format!("{v}:{c}")).collect::<Vec<_>>().join(" ");
        println!(
            "  {:>7} {:>5} {:>6} {:>5}  {:<12} {}",
            p.ppid,
            state,
            total,
            ago((now - p.last_seen).max(0)),
            trunc(&p.cmd, 12),
            breakdown,
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Compact "time ago" for a fixed-width column: `s`/`m`/`h`/`d`.
fn ago(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_verbs_recognized() {
        assert!(is_commit_verb("commit"));
        assert!(is_commit_verb("merge"));
        assert!(is_commit_verb("cherry-pick"));
        assert!(!is_commit_verb("status"));
        assert!(!is_commit_verb("push"));
    }

    #[test]
    fn mutating_verbs_recognized() {
        assert!(is_mutating_verb("commit"));
        assert!(is_mutating_verb("push"));
        assert!(is_mutating_verb("add"));
        assert!(is_mutating_verb("rebase"));
        assert!(!is_mutating_verb("status"));
        assert!(!is_mutating_verb("rev-parse"));
        assert!(!is_mutating_verb("log"));
    }

    #[test]
    fn shell_and_cmd_name() {
        assert!(is_shell("/opt/homebrew/bin/zsh"));
        assert!(is_shell("-zsh")); // login shell
        assert!(is_shell("bash"));
        assert!(!is_shell("claude"));
        assert!(!is_shell("/usr/bin/node"));
        assert_eq!(cmd_name("/opt/homebrew/bin/zsh"), "zsh");
        assert_eq!(cmd_name("-zsh"), "zsh");
        assert_eq!(cmd_name("claude"), "claude");
    }

    #[test]
    fn responsible_process_finds_a_live_ancestor() {
        // From the test process the walk must resolve to a real, live pid (whatever
        // ran the test harness), never 0.
        let (pid, _cmd, _cwd) = responsible_process();
        assert!(pid > 1);
        assert!(is_alive(pid));
    }

    #[test]
    fn alive_current_process_true_and_bogus_false() {
        assert!(is_alive(std::process::id() as i64));
        assert!(!is_alive(0));
        assert!(!is_alive(-5));
    }

    #[test]
    fn trunc_shortens_with_ellipsis() {
        assert_eq!(trunc("short", 22), "short");
        assert_eq!(trunc("aaaaa", 3), "aa…");
    }
}
