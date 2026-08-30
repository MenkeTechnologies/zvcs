//! The daemon's half of `git zpin`, which `zpin_freeze.rs` says it cannot cover:
//! that `react()` actually honours the flag it reads.
//!
//! A pin is per repository — `git zpin <path>` pins each named repository, and
//! the verb documents that "a pinned repo is skipped by autobump and reconcile,
//! so its gitlink and HEAD will not move on their own until it is unpinned".
//! The flag was read once, for the top-level repo, while the reconcile walked
//! the whole tree, so pinning a submodule wrote a row nothing consulted and the
//! next reaction fast-forwarded it anyway. Measured before the fix: a pinned
//! submodule one commit behind its origin was moved onto origin/main.
//!
//! Both directions are asserted in one daemon lifetime, because "it did not
//! move" proves nothing unless the same fixture moves once the pin is lifted.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The daemon isolation one case runs under: its own socket, home and root.
///
/// Handed to each child through [`Command::env`] rather than
/// `std::env::set_var`. `ZVCS_SOCK`/`ZVCS_HOME` are process-wide and the two
/// tests in this file run on parallel threads of one binary, so whichever set
/// them last won for *both* daemons: one bound the other's socket and the loser
/// polled a path nothing would ever create — `daemon socket never appeared at
/// /tmp/zv-pinn<pid>/s`, on a loaded runner, while an idle laptop passed.
///
/// The global and system config are pinned empty for the same reason
/// `autonomy_nested.rs` pins them: a `[zvcs] autohook` in a developer's own
/// `~/.gitconfig` makes `should_watch()` true for these scratch repositories, so
/// the setup commands autostart a detached daemon before the test has built the
/// state it wants to measure — and this test has to own the daemon's lifetime.
struct Isolation {
    root: PathBuf,
    sock: PathBuf,
    home: PathBuf,
}

impl Isolation {
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.env("ZVCS_SOCK", &self.sock)
            .env("ZVCS_HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null");
        cmd
    }

    fn git(&self, dir: &Path, args: &[&str]) -> std::process::Output {
        let out = self
            .cmd()
            .args([
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=zvcs-test",
                "-c",
                "protocol.file.allow=always",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn head(&self, dir: &Path) -> String {
        String::from_utf8_lossy(&self.git(dir, &["rev-parse", "HEAD"]).stdout).trim().to_string()
    }

    /// Poll until `dir`'s HEAD equals `want`, or the timeout elapses.
    fn moved_to(&self, dir: &Path, want: &str, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self.head(dir) == want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        false
    }
}

fn wait_for(path: &Path, timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon socket never appeared at {}", path.display());
}

fn wait_for_log(log: &Path, needle: &str, timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if std::fs::read_to_string(log).map(|s| s.contains(needle)).unwrap_or(false) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The control: the same fixture with no pin at all must reconcile, or the
/// pinned case below proves only that nothing was reconciling.
#[test]
fn an_unpinned_submodule_is_reconciled() {
    assert!(
        reconciles_with(Pin::No),
        "the fixture never reconciled without a pin, so the pinned case would prove nothing"
    );
}

#[test]
fn a_pinned_submodule_is_not_reconciled() {
    assert!(
        !reconciles_with(Pin::Yes),
        "the daemon reconciled a PINNED submodule (the pin was read for the top-level repo only)"
    );
}

#[derive(Clone, Copy, PartialEq)]
enum Pin {
    Yes,
    No,
}

/// Build a parent+submodule tree with the submodule one commit behind its
/// already-fetched origin/main, optionally pin the submodule, run one daemon
/// over it, and report whether the submodule was fast-forwarded.
///
/// Each call owns a fresh tree and a fresh daemon: pinning or unpinning under a
/// daemon that is already watching mixes the two states in one measurement.
fn reconciles_with(pin: Pin) -> bool {
    // Short path: a unix socket must fit in SUN_LEN (~104 bytes).
    let tag = if pin == Pin::Yes { "y" } else { "n" };
    let root = std::env::temp_dir().join(format!("zv-pin{tag}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    // Built before ANY command runs: `git zpin` writes to the database named by
    // ZVCS_HOME, and the daemon reads the one named when it starts. Naming these
    // late puts the pin in one database and the reader on another, and the pin
    // then looks ignored when it was simply never seen.
    let iso = Isolation {
        sock: root.join("s"),
        home: root.join("h"),
        root,
    };
    let root = &iso.root;
    let sock = &iso.sock;

    // A submodule source two commits deep, so its checkout can sit one behind.
    let sub_src = root.join("sub_src");
    std::fs::create_dir_all(&sub_src).unwrap();
    iso.git(&sub_src, &["init", "-q", "-b", "main"]);
    iso.git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "s0"]);
    iso.git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "s1"]);

    let parent = root.join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    iso.git(&parent, &["init", "-q", "-b", "main"]);
    iso.git(&parent, &["commit", "--allow-empty", "-q", "-m", "p0"]);
    iso.git(&parent, &["submodule", "add", "-q", sub_src.to_str().unwrap(), "sub"]);
    iso.git(&parent, &["commit", "-q", "-m", "add sub"]);

    // Put the submodule one commit behind its already-fetched origin/main, which
    // is exactly what the fetch-free reconcile fast-forwards.
    let sub = parent.join("sub");
    iso.git(&sub, &["fetch", "-q", "origin"]);
    let target =
        String::from_utf8_lossy(&iso.git(&sub, &["rev-parse", "origin/main"]).stdout).trim().to_string();
    iso.git(&sub, &["reset", "-q", "--hard", "HEAD~1"]);
    let behind = iso.head(&sub);
    assert_ne!(behind, target, "precondition: the submodule is behind origin/main");

    // Pin BEFORE autonomy is switched on. The daemon runs one reaction the
    // moment it starts, so a pin applied afterwards would be tested against a
    // submodule that had already been reconciled — and `git zpin` itself
    // autostarts a daemon once autonomy is configured, which is the other reason
    // this has to come first.
    if pin == Pin::Yes {
        iso.git(&parent, &["zpin", sub.to_str().unwrap()]);
    }

    // Written directly rather than through `git config`: every zvcs command
    // autostarts the daemon when autonomy is configured, and this test needs to
    // own the daemon's lifetime.
    let mut cfg = std::fs::read_to_string(parent.join(".git/config")).unwrap();
    cfg.push_str("[zvcs]\n\tautoreconcile = true\n\tinterval = 1\n");
    std::fs::write(parent.join(".git/config"), cfg).unwrap();

    let log_path = root.join("daemon.log");
    let logf = std::fs::File::create(&log_path).unwrap();
    let mut daemon: Child = iso
        .cmd()
        .args(["zdaemon", "start", "--foreground"])
        .current_dir(&parent)
        .stdout(Stdio::from(logf.try_clone().unwrap()))
        .stderr(Stdio::from(logf))
        .spawn()
        .expect("spawn zdaemon");
    wait_for(sock, Duration::from_secs(5));
    wait_for_log(&log_path, "[zvcs watch] watching", Duration::from_secs(10));

    std::fs::write(parent.join("poke.txt"), b"one\n").unwrap();
    let moved = iso.moved_to(&sub, &target, Duration::from_secs(20));

    let _ = iso.cmd().args(["zdaemon", "stop"]).current_dir(&parent).status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(root);
    moved
}
