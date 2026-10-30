//! A `receive-pack` hook whose output contains a NUL, relayed to the pusher on
//! side-band 2, compared byte for byte with stock git.
//!
//! `demultiplex_sideband()` reads each packet as a C string (`buf[len] = '\0'`,
//! then `strpbrk()` / `strlen()`, sideband.c:350, :373-416) and
//! `strbuf_add_sanitized()` stops at a NUL (sideband.c:234), so nothing after
//! the first NUL of a packet reaches the terminal — it is never shown as `^@`.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

/// Push `main` into a bare repository whose `post-receive` prints `payload` (a
/// perl string literal) on stderr, which `receive-pack` muxes onto band 2.
fn push(bin: &str, tag: &str, payload: &str) -> (Option<i32>, Vec<u8>, Vec<u8>) {
    let root = std::env::temp_dir().join(format!("zvcs-sbnul-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let git = |cwd: &Path, args: &[&str]| {
        let out = Command::new(STOCK)
            .args(args)
            .current_dir(cwd)
            .env("HOME", &root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .output()
            .unwrap();
        assert!(out.status.success(), "fixture `git {args:?}`");
    };
    let bare = root.join("r.git");
    let work = root.join("w");
    git(&root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    let hook = bare.join("hooks/post-receive");
    std::fs::write(&hook, format!("#!/bin/sh\nexec perl -e 'print \"{payload}\"' >&2\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&root, &["init", "-q", "-b", "main", work.to_str().unwrap()]);
    std::fs::write(work.join("f"), "a\n").unwrap();
    git(&work, &["add", "f"]);
    git(&work, &["commit", "-qm", "a"]);
    // A relative URL, so the `To ...` line is the same for both binaries.
    let out = Command::new(bin)
        .args(["push", "../r.git", "main"])
        .current_dir(&work)
        .env("HOME", &root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
    (out.status.code(), out.stdout, out.stderr)
}

#[test]
fn band_two_stops_at_a_nul() {
    if !Path::new(STOCK).exists() {
        return;
    }
    for (tag, payload) in [
        ("mid", r"a\0b\n"),
        ("alone", r"\0\n"),
        ("second-line", r"x\n\0y\nz\n"),
        ("trailing", r"p\0"),
        ("after-cr", r"q\r\0r\n"),
    ] {
        let want = push(STOCK, &format!("{tag}-stock"), payload);
        let got = push(BIN, &format!("{tag}-zvcs"), payload);
        assert_eq!(
            String::from_utf8_lossy(&got.2),
            String::from_utf8_lossy(&want.2),
            "{tag}: stderr"
        );
        assert_eq!(got.1, want.1, "{tag}: stdout");
        assert_eq!(got.0, want.0, "{tag}: exit code");
        assert!(!want.2.windows(2).any(|w| w == b"^@"), "{tag}: stock never shows the NUL");
    }
}
