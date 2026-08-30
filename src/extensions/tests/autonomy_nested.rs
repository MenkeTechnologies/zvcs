//! The daemon's autonomy pass must reach a submodule nested inside a submodule.
//!
//! `react()` heals detached HEADs across the tree on every coalesced reaction,
//! and the tree it walked stopped at the first level of submodules: a repo two
//! levels down stayed detached forever, silently, in the path nobody watches.
//! The same one-level walk registered the watch targets and ran the config-gated
//! reconcile, so on a nested tree — the normal shape here — a third of the
//! autonomy simply did not apply.
//!
//! Shape follows `autonomy.rs`: an isolated socket and home, the daemon in the
//! foreground with its log captured, and a poll rather than a sleep.
//!
//! The isolation is handed to each child through [`Command::env`] rather than
//! `std::env::set_var`. `ZVCS_SOCK`/`ZVCS_HOME` are process-wide, and the two
//! tests here run on parallel threads of one binary: whichever set them last won
//! for *both* daemons, so one of them bound the other's socket and its
//! `wait_for` timed out on a path nothing was ever going to create. That is the
//! `daemon socket never appeared at …` this file used to fail with on a loaded
//! runner while passing on an idle laptop. Per-child environment removes the
//! shared mutable state instead of serialising around it — the same shape
//! `daemon_ctl.rs` and `watch_rescan.rs` already use.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The daemon isolation one test runs under: its own socket, home and root.
struct Isolation {
    root: std::path::PathBuf,
    sock: std::path::PathBuf,
    home: std::path::PathBuf,
}

impl Isolation {
    /// A `git` invocation carrying this test's isolation, and nothing of the
    /// developer's own daemon or configuration.
    ///
    /// The global and system config are pinned empty as well as the socket and
    /// home. `[zvcs] autohook` in a developer's `~/.gitconfig` makes
    /// `should_watch()` true for *every* repository, so the setup commands
    /// autostart a detached daemon rooted at whichever scratch repo ran first —
    /// which then wins the singleton and watches the wrong tree, and the daemon
    /// this test starts on purpose reports `daemon already running`. A CI runner
    /// has no global config and never sees it; a laptop does.
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

    /// Is this repository on a branch (symbolic HEAD) rather than detached?
    fn attached(&self, dir: &Path) -> bool {
        self.cmd()
            .args(["symbolic-ref", "-q", "HEAD"])
            .current_dir(dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
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
        if let Ok(s) = std::fs::read_to_string(log) {
            if s.contains(needle) {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_reconcile_pass_reaches_a_submodule_two_levels_down() {
    nested_autonomy_heals_the_deepest_repo("zvcs.autoreconcile");
}

/// With only `autobump` on, the config-gated reconcile branch never runs, so the
/// detached HEAD two levels down can be healed by exactly one thing: the attach
/// pass at the top of every reaction. Isolates that walk from the reconcile one.
#[test]
fn the_attach_pass_reaches_a_submodule_two_levels_down() {
    nested_autonomy_heals_the_deepest_repo("zvcs.autobump");
}

fn nested_autonomy_heals_the_deepest_repo(switch: &str) {
    // Short names on purpose: a unix socket path has to fit in SUN_LEN (~104
    // bytes), and a temp dir plus a descriptive directory name overruns it — the
    // daemon then fails to bind and the test looks like an autonomy failure.
    let tag = if switch.contains("reconcile") { "rc" } else { "ab" };
    let root = std::env::temp_dir().join(format!("zv-an{tag}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let iso = Isolation {
        sock: root.join("s"),
        home: root.join("h"),
        root,
    };
    let root = &iso.root;

    // deep ← sub ← parent, each a real local repository.
    let deep_src = root.join("deep_src");
    std::fs::create_dir_all(&deep_src).unwrap();
    iso.git(&deep_src, &["init", "-q", "-b", "main"]);
    iso.git(&deep_src, &["commit", "--allow-empty", "-q", "-m", "deep root"]);

    let sub_src = root.join("sub_src");
    std::fs::create_dir_all(&sub_src).unwrap();
    iso.git(&sub_src, &["init", "-q", "-b", "main"]);
    iso.git(&sub_src, &["commit", "--allow-empty", "-q", "-m", "sub root"]);
    iso.git(&sub_src, &["submodule", "add", "-q", deep_src.to_str().unwrap(), "deep"]);
    iso.git(&sub_src, &["commit", "-q", "-m", "add deep"]);

    let parent = root.join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    iso.git(&parent, &["init", "-q", "-b", "main"]);
    iso.git(&parent, &["commit", "--allow-empty", "-q", "-m", "parent root"]);
    iso.git(&parent, &["submodule", "add", "-q", sub_src.to_str().unwrap(), "sub"]);
    iso.git(&parent, &["commit", "-q", "-m", "add submodule"]);
    iso.git(&parent, &["submodule", "update", "--init", "--recursive"]);

    let deep = parent.join("sub").join("deep");
    assert!(deep.join(".git").exists(), "precondition: the nested submodule is checked out");

    // Detach the nested repo's HEAD — the state the autonomy pass exists to heal.
    iso.git(&deep, &["checkout", "-q", "--detach"]);
    assert!(!iso.attached(&deep), "precondition: the nested submodule is detached");

    iso.git(&parent, &["config", switch, "true"]);
    iso.git(&parent, &["config", "zvcs.interval", "1"]);

    let sock = &iso.sock;
    let daemon_log = root.join("daemon.log");
    let logf = std::fs::File::create(&daemon_log).unwrap();
    let mut daemon: Child = iso
        .cmd()
        .args(["zdaemon", "start", "--foreground"])
        .current_dir(&parent)
        .stdout(Stdio::from(logf.try_clone().unwrap()))
        .stderr(Stdio::from(logf))
        .spawn()
        .expect("spawn zdaemon");
    wait_for(sock, Duration::from_secs(5));
    wait_for_log(&daemon_log, "[zvcs watch] watching", Duration::from_secs(10));

    // Any change in the watched tree coalesces into one reaction, which heals
    // detached HEADs across the whole tree.
    std::fs::write(parent.join("poke.txt"), b"touch\n").unwrap();

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut healed = false;
    while Instant::now() < deadline {
        if iso.attached(&deep) {
            healed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    let _ = iso.cmd().args(["zdaemon", "stop"]).current_dir(&parent).status();
    let _ = daemon.kill();
    let _ = daemon.wait();
    let log = std::fs::read_to_string(&daemon_log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(root);

    assert!(
        healed,
        "the daemon never attached the submodule two levels down (first-level-only tree walk).\ndaemon log:\n{log}"
    );
}
