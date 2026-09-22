//! What `setup_pager()` hands the pager, and which commands reach it at all.
//!
//! `git_pager()` returns a program only when stdout is a terminal
//! (pager.c:95-97), so no pipe can reach the pager at all. Each case therefore
//! opens a pseudo-terminal of its own with `posix_openpt` and gives the child
//! fd 1 and fd 2 on its slave side: the *runner* needs no terminal, no
//! controlling tty and no `test_terminal` — only a kernel that hands out a pty,
//! and where even that is unavailable the case reports a skip rather than
//! failing. The decision logic underneath is pinned a second time, with no pty
//! whatsoever, by `pager.rs`'s own `pager_config_tests`.
//!
//! The pager itself is a shell script that writes its argv and the parts of its
//! environment under test to a file and drains stdin, so every assertion reads
//! that file rather than a screen.
//!
//! The four behaviours pinned, each measured against git 2.55.0 driven the same
//! way, and each one a case this port previously got wrong:
//!
//!   * `GIT_PAGER_IN_USE` is *removed* from the pager's environment, not set in
//!     it (pager.c:189 with run-command.h:72-73).
//!   * `pager.<cmd>` whose value is not a boolean is the pager command itself,
//!     ahead of `core.pager`, and turns paging on for a command that would not
//!     page (pager.c:286-303, 305-317).
//!   * `$COLUMNS` is measured before fd 1 becomes the pipe and exported to the
//!     child (pager.c:167-174).
//!   * `pager.<cmd>` is not consulted for a command `git.c` dispatches without
//!     `RUN_SETUP` (git.c:489), and an empty `$GIT_PAGER` ends the chain rather
//!     than falling through to `$PAGER` (pager.c:100-111).
//!
//! Nothing here asserts on a timing, a pid, or the pager's own output.

use std::io::Read;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A pseudo-terminal opened through POSIX `posix_openpt`/`grantpt`/`unlockpt`,
/// which every Unix libc carries — `openpty()` lives in libutil on some systems
/// and this test must not care which.
struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
}

impl Pty {
    /// Open one, sized `cols` wide so `TIOCGWINSZ` has a real answer to give.
    /// A freshly opened pty reports a zero width, which is exactly the case
    /// git treats as "guessed".
    fn open(cols: u16) -> Option<Pty> {
        // `ptsname()` answers from a static buffer, so two tests opening a pty
        // at once could each be handed the other's slave name. `ptsname_r()` is
        // not portable (it is absent on macOS), so the whole sequence is
        // serialised instead — cargo runs these test functions on threads of
        // one process.
        static SERIALISE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _held = SERIALISE.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the POSIX pty dance on descriptors this function owns
        // outright. Every call is checked; `ptsname` is read under the lock
        // above, before any other call can overwrite its static buffer.
        unsafe {
            let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            if master < 0 {
                return None;
            }
            let master = OwnedFd::from_raw_fd(master);
            let raw = master.as_raw_fd();
            if libc::grantpt(raw) != 0 || libc::unlockpt(raw) != 0 {
                return None;
            }
            let name = libc::ptsname(raw);
            if name.is_null() {
                return None;
            }
            let slave = libc::open(name, libc::O_RDWR | libc::O_NOCTTY);
            if slave < 0 {
                return None;
            }
            let ws = libc::winsize {
                ws_row: 40,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            assert_eq!(
                libc::ioctl(slave, libc::TIOCSWINSZ as _, &ws),
                0,
                "a pty that opened must accept TIOCSWINSZ"
            );
            Some(Pty {
                master,
                slave: OwnedFd::from_raw_fd(slave),
            })
        }
    }

    /// A fresh descriptor onto the slave side, for one of the child's streams.
    fn slave_stdio(&self) -> Stdio {
        self.slave.try_clone().expect("dup the pty slave").into()
    }
}

/// Run the binary with fd 1 and fd 2 on a `cols`-wide pty, and wait for it.
///
/// The master side is drained on a thread: a pty's buffer is small, and a
/// command whose output outruns it would otherwise block forever. The drained
/// bytes are the terminal's, not the pager's, so they are dropped.
///
/// `false` when this machine hands out no pty at all — a sandbox with no
/// `/dev/ptmx`. The runner's *own* terminal is never needed, only the ability
/// to open one; the decision logic these cases cover is additionally pinned
/// without any pty by `pager.rs`'s own `pager_config_tests`.
#[must_use]
fn run_on_pty(repo: &Path, cols: u16, env: &[(&str, &str)], args: &[&str]) -> bool {
    let Some(pty) = Pty::open(cols) else {
        return false;
    };
    let home = repo.join(".isolated-home");
    std::fs::create_dir_all(&home).unwrap();

    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(repo)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("LESS")
        .env_remove("LV")
        .env_remove("COLUMNS")
        .env_remove("GIT_PAGER")
        .env_remove("PAGER")
        .env_remove("GIT_PAGER_IN_USE")
        .stdin(Stdio::null())
        .stdout(pty.slave_stdio())
        .stderr(pty.slave_stdio());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn the binary");

    // Every copy of the slave in this process has to go before the reader can
    // ever see EOF — including the ones `Command` is still holding: `spawn()`
    // hands the child duplicates and keeps the originals until the `Command`
    // itself is dropped.
    drop(cmd);
    drop(pty.slave);
    let master: RawFd = {
        use std::os::unix::io::AsRawFd;
        pty.master.as_raw_fd()
    };
    let drain = std::thread::spawn(move || {
        // SAFETY: the master fd outlives this thread — it is joined below,
        // before `pty.master` is dropped.
        let mut f = unsafe { std::fs::File::from_raw_fd(master) };
        let mut sink = Vec::new();
        let _ = f.read_to_end(&mut sink);
        std::mem::forget(f);
    });
    let _ = child.wait();
    let _ = drain.join();
    drop(pty.master);
    true
}

/// Report a machine that hands out no pty, so the skip is visible in the log
/// rather than silent.
fn skipped(case: &str) {
    eprintln!("skipping {case}: this machine opened no pseudo-terminal");
}

/// A pager script that records the environment under test and swallows stdin.
///
/// `<unset>` is written for a variable that is absent, so "absent" and "empty"
/// are distinguishable in the recording — which is the whole point of the
/// `GIT_PAGER_IN_USE` case.
fn recorder(dir: &Path, name: &str, out: &Path) -> PathBuf {
    let script = dir.join(name);
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             {{\n\
             echo \"WHO={name}\"\n\
             for a in \"$@\"; do echo \"ARG=[$a]\"; done\n\
             echo \"GIT_PAGER_IN_USE=[${{GIT_PAGER_IN_USE-<unset>}}]\"\n\
             echo \"COLUMNS=[${{COLUMNS-<unset>}}]\"\n\
             echo \"LESS=[${{LESS-<unset>}}]\"\n\
             }} > '{out}'\n\
             cat >/dev/null\n",
            out = out.display()
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&script, perms).unwrap();
    script
}

fn git(dir: &Path, args: &[&str]) {
    let home = dir.join(".isolated-home");
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fixture(tag: &str) -> PathBuf {
    let repo = std::env::temp_dir().join(format!("zvcs-pagerenv-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    std::fs::write(repo.join("f"), "one\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    repo
}

/// The pager child must not inherit `GIT_PAGER_IN_USE`.
///
/// `setup_pager()` sets the flag for *its own* process (pager.c:176) and then
/// pushes the bare name onto the child's env strvec (pager.c:189), which
/// `prepare_env()` reads as a deletion (run-command.c:482-484, documented at
/// run-command.h:72-73). A pager that is itself a script running git — the
/// common `PAGER='delta'`, `PAGER='diff-so-fancy | less'` shape — would
/// otherwise see the flag and believe its own output was already paged.
///
/// This port exported `GIT_PAGER_IN_USE=true` to the child instead.
#[test]
fn the_pager_child_does_not_inherit_the_in_use_flag() {
    let repo = fixture("inuse");
    let out = repo.join("rec.txt");
    let pager = recorder(&repo, "rec.sh", &out);

    if !run_on_pty(
        &repo,
        80,
        &[("GIT_PAGER", pager.to_str().unwrap())],
        &["log"],
    ) {
        return skipped("the pager child's environment");
    }

    let recorded = std::fs::read_to_string(&out).expect("the pager ran and recorded");
    assert!(
        recorded.contains("GIT_PAGER_IN_USE=[<unset>]"),
        "the flag must be removed from the pager's environment, recorded:\n{recorded}"
    );
    // The build-time PAGER_ENV defaults are still applied, so this is not
    // passing by way of the child getting no environment at all.
    assert!(
        recorded.contains("LESS=[FRX]"),
        "LESS must still be defaulted for the child, recorded:\n{recorded}"
    );
    let _ = std::fs::remove_dir_all(&repo);
}

/// `pager.<cmd>` whose value is not a boolean is the pager command.
///
/// `pager_command_config()` (pager.c:286-303) parses the value as a boolean and,
/// when that fails, sets `data->want = 1` *and* keeps the string;
/// `check_pager_config()` (pager.c:315-316) stores it in `pager_program`, which
/// `git_pager()` (pager.c:103-107) reaches before `core.pager`. So the value
/// both turns paging on for a command that does not page by default and wins
/// the program chain.
///
/// This port read the key with a boolean parser only: a command value parsed as
/// "not specified", so the command fell back to the default-paging set and to
/// `core.pager` for the program.
#[test]
fn a_pager_command_config_value_is_the_pager_and_turns_paging_on() {
    let repo = fixture("cmdvalue");
    let chosen = repo.join("chosen.txt");
    let wanted = recorder(&repo, "wanted.sh", &chosen);
    let core = recorder(&repo, "core.sh", &chosen);

    // `rev-list` is not in the default-paging set, so paging at all is the
    // config value's doing; `core.pager` is set to the other recorder, so which
    // file content appears says which program won.
    if !run_on_pty(
        &repo,
        80,
        &[],
        &[
            "-c",
            &format!("core.pager={}", core.display()),
            "-c",
            &format!("pager.rev-list={}", wanted.display()),
            "rev-list",
            "HEAD",
        ],
    ) {
        return skipped("pager.<cmd> as the pager command");
    }

    let recorded = std::fs::read_to_string(&chosen)
        .expect("pager.rev-list with a command value must page rev-list");
    assert!(
        recorded.contains("WHO=wanted.sh"),
        "pager.<cmd> must win over core.pager, recorded:\n{recorded}"
    );

    // ...but only while the command line has not already decided. git.c:489
    // guards the whole config step with `use_pager == -1`, so `-p` skips
    // `check_pager_config()` and with it the assignment to `pager_program` —
    // leaving `core.pager` to win the chain.
    std::fs::remove_file(&chosen).unwrap();
    assert!(run_on_pty(
        &repo,
        80,
        &[],
        &[
            "-p",
            "-c",
            &format!("core.pager={}", core.display()),
            "-c",
            &format!("pager.rev-list={}", wanted.display()),
            "rev-list",
            "HEAD",
        ],
    ), "a pty opened once must open again");
    let recorded = std::fs::read_to_string(&chosen).expect("-p pages rev-list");
    assert!(
        recorded.contains("WHO=core.sh"),
        "-p must skip the pager.<cmd> lookup, recorded:\n{recorded}"
    );
    let _ = std::fs::remove_dir_all(&repo);
}

/// The terminal width is measured before fd 1 becomes the pipe, and exported.
///
/// pager.c:167-174 grabs `term_columns()` and `setenv("COLUMNS", …, 0)` with
/// the comment that says why: after the redirect there is no window left to
/// `ioctl`. The width here comes from `TIOCSWINSZ` on the pty, so the assertion
/// is on a number this test chose, not on a screen.
///
/// This port had no `TIOCGWINSZ` probe and exported nothing, so a pager and any
/// grandchild of it saw no `COLUMNS` at all.
#[test]
fn the_measured_terminal_width_reaches_the_pager_as_columns() {
    let repo = fixture("columns");
    let out = repo.join("rec.txt");
    let pager = recorder(&repo, "rec.sh", &out);

    if !run_on_pty(
        &repo,
        137,
        &[("GIT_PAGER", pager.to_str().unwrap())],
        &["log"],
    ) {
        return skipped("the exported terminal width");
    }

    let recorded = std::fs::read_to_string(&out).expect("the pager ran and recorded");
    assert!(
        recorded.contains("COLUMNS=[137]"),
        "the pty's own width must be exported, recorded:\n{recorded}"
    );
    let _ = std::fs::remove_dir_all(&repo);
}

/// A command `git.c` dispatches without `RUN_SETUP` never reads `pager.<cmd>`.
///
/// git.c:489-491 gates `check_pager_config()` on `run_setup`, so for the
/// no-setup entries — `{ "rev-parse", cmd_rev_parse, NO_PARSEOPT }` (git.c:648),
/// `{ "help", cmd_help }` (git.c:589) — the key is never read and the command
/// runs unpaged whatever it says.
///
/// This port consulted `pager.<cmd>` for every verb and additionally carried
/// `help` in its default-paging set, so both of these paged.
#[test]
fn pager_config_does_not_reach_a_command_dispatched_without_run_setup() {
    let repo = fixture("nosetup");
    let out = repo.join("rec.txt");
    let pager = recorder(&repo, "rec.sh", &out);
    let pager_env = [("GIT_PAGER", pager.to_str().unwrap())];

    if !run_on_pty(
        &repo,
        80,
        &pager_env,
        &["-c", "pager.rev-parse=true", "rev-parse", "HEAD"],
    ) {
        return skipped("pager config on a no-setup command");
    }
    assert!(
        !out.exists(),
        "pager.rev-parse must not page rev-parse, recorded:\n{}",
        std::fs::read_to_string(&out).unwrap_or_default()
    );

    assert!(run_on_pty(
        &repo,
        80,
        &pager_env,
        &["-c", "pager.help=true", "help"]
    ), "a pty opened once must open again");
    assert!(
        !out.exists(),
        "pager.help must not page help, recorded:\n{}",
        std::fs::read_to_string(&out).unwrap_or_default()
    );

    // The gate is `run_setup`, not "this port declines to page": `-p` still
    // pages the same command, so the assertions above are about the config
    // lookup rather than about paging being switched off wholesale.
    assert!(
        run_on_pty(&repo, 80, &pager_env, &["-p", "rev-parse", "HEAD"]),
        "a pty opened once must open again"
    );
    assert!(
        out.exists(),
        "-p must still page a command whose pager config is ignored"
    );
    let _ = std::fs::remove_dir_all(&repo);
}

/// An empty `$GIT_PAGER` means "no pager", not "ask `$PAGER`".
///
/// `git_pager()` walks the chain on a NULL `getenv` only (pager.c:100-108) and
/// collapses the result at the end: `if (!*pager || !strcmp(pager, "cat"))
/// pager = NULL` (pager.c:109-110). An empty `GIT_PAGER` therefore stops the
/// chain and disables paging; it does not hand the decision to `$PAGER`.
///
/// This port treated an empty value as an absent one and walked on, so
/// `GIT_PAGER= PAGER=less git log` paged where git does not.
#[test]
fn an_empty_git_pager_stops_the_chain_rather_than_falling_through() {
    let repo = fixture("emptyenv");
    let out = repo.join("rec.txt");
    let pager = recorder(&repo, "rec.sh", &out);

    if !run_on_pty(
        &repo,
        80,
        &[("GIT_PAGER", ""), ("PAGER", pager.to_str().unwrap())],
        &["log"],
    ) {
        return skipped("the empty GIT_PAGER chain");
    }
    assert!(
        !out.exists(),
        "an empty GIT_PAGER must disable paging, recorded:\n{}",
        std::fs::read_to_string(&out).unwrap_or_default()
    );

    // The same run with GIT_PAGER simply absent does reach $PAGER, so the
    // assertion above is about emptiness and not about $PAGER being ignored.
    assert!(
        run_on_pty(&repo, 80, &[("PAGER", pager.to_str().unwrap())], &["log"]),
        "a pty opened once must open again"
    );
    assert!(out.exists(), "an absent GIT_PAGER must fall through to $PAGER");
    let _ = std::fs::remove_dir_all(&repo);
}
