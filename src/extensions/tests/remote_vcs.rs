//! `remote.<name>.vcs` — the foreign-SCM helper a remote is reached through, and
//! this port's documented refusal to pretend it can drive one.
//!
//! In git the key is recorded by `remote_get_1()` as `remote->foreign_vcs`
//! (remote.c:571-573), and `transport_get()` (transport.c:1239, :1251-1253) then
//! hands the *whole* connection to `git-remote-<vcs>` — the URL's own scheme is
//! never consulted:
//!
//! ```c
//! helper = remote->foreign_vcs;
//! …
//! if (helper) {
//!         transport_helper_init(ret, helper);
//! ```
//!
//! This port has no `transport-helper.c`, so instead of ignoring the key and
//! quietly connecting to the URL with the git protocol — which for a real
//! `[remote "hg"] vcs = hg` would fail much later with a diagnostic about the wrong
//! thing — it reads the key and refuses up front, in one line, exit 128.
//!
//! The fixture is what makes these tests worth anything: the remote's URL is a
//! local path that **is** a perfectly good git repository, so the git-protocol
//! fetch behind it demonstrably works. Each refusal test is paired with a control
//! on the same URL with the key unset, which must *succeed* and produce refs. An
//! implementation that stopped reading `remote.<name>.vcs` would pass no test here:
//! the refusal tests would fetch successfully instead of refusing.
//!
//! Stock git is deliberately **not** compared against in this file. It would run
//! `git-remote-hg`, which is a divergence this port documents rather than
//! reproduces — git 2.55.0 answers `git: 'remote-hg' is not a git command.` plus
//! `fatal: remote helper 'hg' aborted session`, which is a statement about the
//! helper being uninstalled, not about the URL.
//!
//! Nothing here touches the network: every URL is a filesystem path.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Identity and date vars git honors above config; pinned so a CI job that exports
/// them cannot change what these fixtures commit.
const PINNED: [&str; 6] = [
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_DATE",
];

/// A scratch `$HOME` shared by every command in this file, outside any fixture.
/// This port writes a cache directory and a sqlite database under `$HOME`; with
/// `HOME` pointing into a repository those land in its worktree and show up in the
/// `git status` these tests read.
fn home() -> PathBuf {
    let p = std::env::temp_dir().join(format!("zvcs-remotevcs-home-{}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn git(dir: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(BIN);
    for v in PINNED {
        c.env_remove(v);
    }
    c.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        // A scratch HOME outside every fixture: this port keeps its own cache and
        // sqlite db under $HOME, and pointing that at a repository would leave
        // untracked files in the very worktree these tests inspect.
        .env("HOME", home())
        .env("ZVCS_HOME", home())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
        .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000")
        .output()
        .unwrap()
}

fn ok(dir: &Path, args: &[&str]) {
    let out = git(dir, args);
    assert!(out.status.success(), "git {args:?} failed: {}", err(&out));
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn err(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn out_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// A local repository `up` with one commit on `main`, and a second repository `r`
/// whose remote `hg` points at `up` by filesystem path — a URL the git protocol
/// can read, which is what makes the refusal meaningful.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-remotevcs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    let up = root.join("up");
    std::fs::create_dir_all(&up).unwrap();
    ok(&up, &["init", "-q", "-b", "main", "."]);
    std::fs::write(up.join("f"), "a\n").unwrap();
    ok(&up, &["add", "f"]);
    ok(&up, &["commit", "-q", "-m", "base"]);

    let r = root.join("r");
    std::fs::create_dir_all(&r).unwrap();
    ok(&r, &["init", "-q", "-b", "main", "."]);
    std::fs::write(r.join("x"), "x\n").unwrap();
    ok(&r, &["add", "x"]);
    ok(&r, &["commit", "-q", "-m", "local"]);
    ok(&r, &["remote", "add", "hg", up.to_str().unwrap()]);
    (r, up)
}

/// The single line the refusal prints. `<name>` is the remote's name as written on
/// the command line and `<vcs>` the configured value, both echoed back, and the
/// helper program named after it is the one git would have run.
fn refusal(name: &str, vcs: &str) -> String {
    format!("fatal: remote.{name}.vcs={vcs} needs the git-remote-{vcs} helper protocol, which is not ported\n")
}

fn remote_refs(dir: &Path) -> String {
    let out = git(dir, &["for-each-ref", "--format=%(refname)", "refs/remotes"]);
    assert!(out.status.success(), "{}", err(&out));
    out_str(&out)
}

/// Control: with the key unset, both commands work over this remote's URL. Without
/// this, every refusal below could be explained by a broken fixture rather than by
/// the config being read.
#[test]
fn the_same_remote_works_when_the_key_is_unset() {
    let (r, up) = fixture("control");
    let tip = {
        let out = git(&up, &["rev-parse", "HEAD"]);
        out_str(&out).trim().to_string()
    };

    let ls = git(&r, &["ls-remote", "hg"]);
    assert_eq!(code(&ls), 0, "{}", err(&ls));
    assert!(
        out_str(&ls).contains(&format!("{tip}\trefs/heads/main")),
        "ls-remote stdout: {:?}",
        out_str(&ls)
    );

    let fetch = git(&r, &["fetch", "hg"]);
    assert_eq!(code(&fetch), 0, "{}", err(&fetch));
    assert!(remote_refs(&r).contains("refs/remotes/hg/main"), "refs: {:?}", remote_refs(&r));
}

/// `git ls-remote <name>` refuses before connecting, on stderr, with nothing on
/// stdout — no partially-listed advertisement, no URL echo.
#[test]
fn ls_remote_refuses_a_remote_with_a_vcs_helper() {
    let (r, _up) = fixture("lsremote");
    ok(&r, &["config", "remote.hg.vcs", "hg"]);
    let out = git(&r, &["ls-remote", "hg"]);

    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(err(&out), refusal("hg", "hg"));
    assert_eq!(out_str(&out), "", "the refusal precedes any advertisement");
}

/// `git fetch <name>` refuses the same way, and — since the refusal precedes the
/// connection — writes no remote-tracking refs at all.
#[test]
fn fetch_refuses_a_remote_with_a_vcs_helper() {
    let (r, _up) = fixture("fetch");
    ok(&r, &["config", "remote.hg.vcs", "hg"]);
    let out = git(&r, &["fetch", "hg"]);

    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(err(&out), refusal("hg", "hg"));
    assert_eq!(out_str(&out), "");
    assert_eq!(remote_refs(&r), "", "nothing was fetched");
    assert!(!r.join(".git/FETCH_HEAD").exists(), "FETCH_HEAD must not be written");
}

/// The value is echoed verbatim into both halves of the message, so the helper
/// named is the one that would have been run. A one-off `-c` reaches the same read
/// as a written config entry.
#[test]
fn the_configured_helper_name_is_echoed_back() {
    for vcs in ["hg", "bzr", "svn"] {
        let (r, _up) = fixture(&format!("name-{vcs}"));
        let out = git(&r, &["-c", &format!("remote.hg.vcs={vcs}"), "fetch", "hg"]);

        assert_eq!(code(&out), 128, "{vcs}: {}", err(&out));
        assert_eq!(err(&out), refusal("hg", vcs), "{vcs}");
    }
}

/// The remote's own name is echoed too, not a hard-coded one: a remote called
/// `origin` reports `remote.origin.vcs`.
#[test]
fn the_remote_name_in_the_message_is_the_remote_that_was_named() {
    let (r, up) = fixture("othername");
    ok(&r, &["remote", "add", "origin", up.to_str().unwrap()]);
    ok(&r, &["config", "remote.origin.vcs", "bzr"]);
    let out = git(&r, &["fetch", "origin"]);

    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(err(&out), refusal("origin", "bzr"));
}

/// The key is per-remote, and only the named remote's subsection is consulted: with
/// two remotes on the *same* URL, the one carrying the key is refused and the other
/// fetches. This is what rules out a global "any `vcs` key anywhere" read.
#[test]
fn the_key_is_scoped_to_the_named_remote() {
    let (r, up) = fixture("scoped");
    ok(&r, &["remote", "add", "plain", up.to_str().unwrap()]);
    ok(&r, &["config", "remote.hg.vcs", "hg"]);

    let refused = git(&r, &["fetch", "hg"]);
    assert_eq!(code(&refused), 128, "{}", err(&refused));
    assert_eq!(err(&refused), refusal("hg", "hg"));

    let allowed = git(&r, &["fetch", "plain"]);
    assert_eq!(code(&allowed), 0, "the sibling remote on the same URL must still fetch: {}", err(&allowed));
    let refs = remote_refs(&r);
    assert!(refs.contains("refs/remotes/plain/main"), "refs: {refs:?}");
    assert!(!refs.contains("refs/remotes/hg/"), "refs: {refs:?}");
}

/// Config subsection names are case-sensitive, so `remote.HG.vcs` is a setting for
/// a remote called `HG` and says nothing about `hg`. Measured, not assumed: the
/// fetch below succeeds.
#[test]
fn the_subsection_name_is_case_sensitive() {
    let (r, _up) = fixture("case");
    let out = git(&r, &["-c", "remote.HG.vcs=hg", "fetch", "hg"]);

    assert_eq!(code(&out), 0, "remote.HG.vcs must not apply to the remote 'hg': {}", err(&out));
    assert!(remote_refs(&r).contains("refs/remotes/hg/main"));
}

/// A URL given directly on the command line has no remote name, so there is no
/// subsection to look the key up in — the same `remote.hg.vcs` that refuses `git
/// fetch hg` cannot refuse `git ls-remote <path>`.
#[test]
fn a_url_argument_carries_no_remote_name_to_look_up() {
    let (r, up) = fixture("url");
    ok(&r, &["config", "remote.hg.vcs", "hg"]);
    let out = git(&r, &["ls-remote", up.to_str().unwrap()]);

    assert_eq!(code(&out), 0, "{}", err(&out));
    assert!(out_str(&out).contains("refs/heads/main"), "stdout: {:?}", out_str(&out));
}

/// An **empty** value is configured, not absent. `git_config_string()` stores
/// `""` on `remote->foreign_vcs`, so `transport_get()`'s `if (helper)` is still
/// true and stock reaches for a helper named `git-remote-` rather than opening
/// the URL:
///
/// ```text
/// $ git -c remote.hg.vcs= ls-remote hg
/// git: 'remote-' is not a git command. See 'git --help'.
/// fatal: remote helper '' aborted session
/// ```
///
/// The port refuses instead of connecting, with the empty helper name echoed —
/// what matters is that it does not silently fall back to the git protocol,
/// which is exactly what an `is_empty()` filter on the read would cause.
#[test]
fn an_empty_vcs_value_is_configured_not_unset() {
    let (r, _up) = fixture("empty");
    ok(&r, &["config", "remote.hg.vcs", ""]);

    let out = git(&r, &["ls-remote", "hg"]);
    assert_eq!(code(&out), 128, "{}", err(&out));
    assert_eq!(err(&out), refusal("hg", ""));
    assert_eq!(out_str(&out), "", "an empty value connected over the git protocol");

    let fetch = git(&r, &["fetch", "hg"]);
    assert_eq!(code(&fetch), 128, "{}", err(&fetch));
    assert_eq!(remote_refs(&r), "", "an empty value fetched anyway");
}
