//! `git archive --format=zip`, compared byte for byte against stock git 2.50.1.
//!
//! A zip container is all header fields, and git's choices in them are not the
//! obvious ones: "version needed" stays 10 even for a deflated entry, an entry is
//! only deflated when that actually shrinks it, the extended-timestamp extra goes
//! in *both* headers, the UTF-8 name flag appears only for a non-ASCII path, and
//! zip's "apparently text" internal bit is off for binary content. Each of those
//! is one or two bytes that no functional test would notice, so this compares the
//! whole file against stock's.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const STOCK: &str = "/opt/homebrew/bin/git";

fn run_with(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run binary")
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let o = run_with(BIN, dir, home, args);
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

/// A tree with every entry shape whose header fields differ: a stored file, a
/// compressible one, binary content, a directory, a symlink, an executable and a
/// non-ASCII name.
fn fixture() -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("small.txt"), "hello").unwrap();
    std::fs::write(repo.join("compressible.txt"), "a".repeat(400) + "\n").unwrap();
    std::fs::write(repo.join("binary.bin"), (0u8..=255).cycle().take(4096).collect::<Vec<_>>())
        .unwrap();
    std::fs::write(repo.join("sub/nested.txt"), "nested\n").unwrap();
    std::fs::write(repo.join("ünïcode.txt"), "u").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("small.txt", repo.join("link")).unwrap();
        let exec = repo.join("run.sh");
        std::fs::write(&exec, "#!/bin/sh\ntrue\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-q", "-m", "c1"]);
    (root, repo, home)
}

#[test]
fn zip_archives_are_byte_identical_to_git() {
    if !Path::new(STOCK).exists() {
        eprintln!("skipping: {STOCK} not installed");
        return;
    }
    let (root, repo, home) = fixture();

    // `--mtime` pins the one field that would otherwise be the wall clock, so a
    // tree-ish without a commit compares too.
    let cases: Vec<Vec<&str>> = vec![
        vec!["archive", "--format=zip", "HEAD"],
        vec!["archive", "--format=zip", "-0", "HEAD"],
        vec!["archive", "--format=zip", "-1", "HEAD"],
        vec!["archive", "--format=zip", "-9", "HEAD"],
        vec!["archive", "--format=zip", "--prefix=pre/", "HEAD"],
        vec!["archive", "--format=zip", "HEAD", "--", "sub"],
        vec!["archive", "--format=zip", "--mtime=@1600000000", "HEAD"],
        vec!["archive", "--format=zip", "--mtime=@1700000000", "HEAD^{tree}"],
        vec!["archive", "--format=zip", "--add-virtual-file=v.txt:hi", "HEAD"],
    ];
    for args in &cases {
        let mine = run_with(BIN, &repo, &home, args);
        let theirs = run_with(STOCK, &repo, &home, args);
        assert_eq!(
            mine.status.code(),
            theirs.status.code(),
            "exit code for {args:?}: {}",
            String::from_utf8_lossy(&mine.stderr)
        );
        assert_eq!(
            mine.stdout.len(),
            theirs.stdout.len(),
            "archive size for {args:?} ({} vs {})",
            mine.stdout.len(),
            theirs.stdout.len()
        );
        assert!(
            mine.stdout == theirs.stdout,
            "archive bytes differ for {args:?} at offset {:?}",
            mine.stdout.iter().zip(&theirs.stdout).position(|(a, b)| a != b)
        );
    }

    // The container is a real zip: the comment is the commit id, and the entry
    // whose name is not ASCII carries the UTF-8 flag.
    let z = run_with(BIN, &repo, &home, &["archive", "--format=zip", "HEAD"]).stdout;
    let head = String::from_utf8_lossy(
        &run_with(BIN, &repo, &home, &["rev-parse", "HEAD"]).stdout,
    )
    .trim_end()
    .to_string();
    assert!(z.ends_with(head.as_bytes()), "the archive comment is the commit id");
    assert_eq!(&z[..4], b"PK\x03\x04");

    let _ = std::fs::remove_dir_all(&root);
}
