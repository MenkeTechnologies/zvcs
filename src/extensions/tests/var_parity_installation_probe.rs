//! `GIT_ATTR_SYSTEM` and `GIT_CONFIG_SYSTEM` are `system_path(ETC_GITATTRIBUTES)` and
//! `system_path(ETC_GITCONFIG)` (attr.c:874-880, config.c:1512), constants compiled into a
//! git build. zvcs stands in for the git installation it shadows and asks the first
//! other `git` on `PATH` for them.
//!
//! It used to ask only where that git's highest-scoped `gitconfig` lives. git's own
//! `make install` writes no `etc/gitconfig`, so an installation built from source
//! answered nothing and the next `git` on `PATH` was taken instead: CI's stock git in
//! `~/.local/stock-git` printed `~/.local/stock-git/etc/gitattributes` while zvcs printed
//! `/etc/gitattributes`, the system git's. Here the first `git` on `PATH` is stock git
//! with its system config switched off, and a second one claims another installation.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn script(dir: &Path, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let git = dir.join("git");
    std::fs::write(&git, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn var(bin: &str, root: &Path, path: &str, name: &str) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(["var", name])
        .current_dir(root)
        .env_clear()
        .env("PATH", path)
        .env("HOME", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("LC_ALL", "C")
        .output()
        .expect("run git var");
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (out.status.code().unwrap_or(-1), text(&out.stdout), text(&out.stderr))
}

#[test]
fn the_first_git_on_path_answers_even_without_a_system_gitconfig() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let root = std::env::temp_dir().join(format!("zvcs-var-install-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // An installation that ships no system gitconfig, then one that claims `/elsewhere`.
    let first = root.join("first");
    let second = root.join("second");
    script(&first, &format!("GIT_CONFIG_NOSYSTEM=1 exec '{stock}' \"$@\""));
    script(
        &second,
        "[ \"$1\" = config ] || exit 129\nprintf 'file:/elsewhere/etc/gitconfig\\000core.x\\000'",
    );
    let path = format!("{}:{}:/usr/bin:/bin", first.display(), second.display());
    for name in ["GIT_ATTR_SYSTEM", "GIT_CONFIG_SYSTEM"] {
        let want = var(stock, &root, &path, name);
        assert_eq!(want.0, 0, "{want:?}");
        assert_eq!(var(ZVCS, &root, &path, name), want, "{name}");
    }

    // A git that cannot answer `var GIT_ATTR_SYSTEM` is asked for its gitconfig.
    let path = format!("{}:/usr/bin:/bin", second.display());
    let got = var(ZVCS, &root, &path, "GIT_ATTR_SYSTEM");
    assert_eq!(got, (0, "/elsewhere/etc/gitattributes\n".into(), String::new()));
    let _ = std::fs::remove_dir_all(&root);
}
