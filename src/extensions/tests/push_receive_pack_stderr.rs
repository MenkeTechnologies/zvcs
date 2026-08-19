//! A local `git push` must let `git-receive-pack`'s stderr reach the pusher.
//!
//! git's local branch of `git_connect()` (connect.c:1479-1491) fills the child and calls
//! `start_command(conn)` without ever setting `no_stderr`, so the service inherits the
//! caller's stderr. That is the channel `advice.ignoredHook` uses — `find_hook()`
//! (hook.c:48-62) `advise()`s about a hook that exists but is not executable — and the
//! channel anything a `--receive-pack` wrapper writes uses. Sending it to `/dev/null`
//! discarded every one of those diagnostics while the push otherwise succeeded.
//!
//! Unix-only: the transport is pointed at the binary under test with symlinks, and the
//! wrapper case needs an executable shell script.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    bindir: PathBuf,
    src: PathBuf,
    remote: PathBuf,
}

impl Fixture {
    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.bindir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(BIN)
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
            .env("PATH", path)
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.home.join("gitsystem"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run binary")
    }

    fn ok(&self, cwd: &Path, args: &[&str]) {
        let out = self.run(cwd, args);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn fixture(tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-push-rp-stderr-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let fx = Fixture {
        home: root.join("home"),
        bindir: root.join("bin"),
        src: root.join("src"),
        remote: root.join("remote.git"),
        root,
    };
    for dir in [&fx.home, &fx.bindir, &fx.src] {
        std::fs::create_dir_all(dir).unwrap();
    }
    // The transport looks the service up on `PATH`; serve it with the binary under test.
    for name in ["git", "git-receive-pack", "git-upload-pack"] {
        std::os::unix::fs::symlink(BIN, fx.bindir.join(name)).unwrap();
    }

    fx.ok(&fx.src, &["init", "-q", "--bare", "-b", "main", fx.remote.to_str().unwrap()]);
    fx.ok(&fx.src, &["init", "-q", "-b", "main", "."]);
    fx.ok(&fx.src, &["config", "user.email", "t@e.co"]);
    fx.ok(&fx.src, &["config", "user.name", "t"]);
    std::fs::write(fx.src.join("a.txt"), "a\n").unwrap();
    fx.ok(&fx.src, &["add", "a.txt"]);
    fx.ok(&fx.src, &["commit", "-q", "-m", "one"]);
    fx
}

#[test]
fn a_non_executable_hook_advises_the_pusher() {
    let fx = fixture("hook");
    let hook = fx.remote.join("hooks").join("pre-receive");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
    // Deliberately not executable: that is exactly the state `find_hook()` advises about.
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o644)).unwrap();

    let out = fx.run(&fx.src, &["push", fx.remote.to_str().unwrap(), "main"]);
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("hook was ignored because it's not set as executable."),
        "receive-pack's advice never reached the pusher; stderr was: {err}"
    );
    assert!(
        err.contains("You can disable this warning with `git config set advice.ignoredHook false`."),
        "the advice lost its second line; stderr was: {err}"
    );
    // The push itself still has to have happened.
    let out = fx.run(&fx.remote, &["rev-parse", "refs/heads/main"]);
    assert!(out.status.success(), "the ref never landed on the remote");
}

#[test]
fn a_receive_pack_wrapper_keeps_its_stderr() {
    let fx = fixture("wrapper");
    let wrapper = fx.root.join("rp.sh");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\necho 'MARKER-FROM-RECEIVE-PACK' >&2\nexec '{BIN}' receive-pack \"$@\"\n"),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = fx.run(
        &fx.src,
        &[
            "push",
            &format!("--receive-pack={}", wrapper.display()),
            fx.remote.to_str().unwrap(),
            "main",
        ],
    );
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("MARKER-FROM-RECEIVE-PACK"),
        "the wrapper's stderr was discarded; stderr was: {err}"
    );
}
