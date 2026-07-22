//! `git archive` reads exactly one config key that changes its byte output:
//! `tar.umask`, the permission mask git ANDs into every tar entry's mode
//! (`archive-tar.c: git_tar_config` / `write_entry`). It has no command-line
//! override — git exposes no `--umask` flag — so the config value is the sole
//! driver. These tests pin the ported behavior byte-for-byte against stock git:
//! a numeric mask, the special `tar.umask=user` (which git reads from the
//! process umask via `umask(0)`-then-restore), and the default (0002) when the
//! key is unset. They also assert distinct masks yield distinct archives, so an
//! all-empty / all-erroring implementation cannot pass by producing two equal
//! empty streams.
//!
//! The other archive-related keys are deliberately not tested here because they
//! drive no behavior this port implements: `tar.<format>.command` needs an
//! external filter subprocess (git spawns it; this port rejects it),
//! `tar.<format>.remote` only gates which formats the git-upload-archive server
//! offers (no effect on a local archive — verified identical bytes for
//! true/false/unset), and `uploadarchive.allowUnreachable` belongs to the
//! `--remote` transport, which this port does not drive.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repo with one committed regular file at a known mode (0644) and a fixed
/// commit date, so the archive's entry mtime and pax global header are
/// deterministic and identical between the two binaries.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-archivecfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    let f = repo.join("f.txt");
    std::fs::write(&f, "hi\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    git(&repo, &["add", "f.txt"]);
    assert!(
        Command::new("git")
            .args(["commit", "-q", "-m", "c"])
            .current_dir(&repo)
            .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
            .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
            .status()
            .unwrap()
            .success(),
        "commit failed"
    );
    (repo, home)
}

/// Run `<bin> archive --format=tar HEAD` in the repo with a hermetic
/// environment (empty HOME, no system config, `ZVCS_HOME` for the port). Both
/// binaries read `tar.umask` from the repo's own `.git/config`, so no `-c`
/// override is needed — which matters because this port has no `-c` support.
fn archive_tar(bin: &str, repo: &Path, home: &Path) -> Vec<u8> {
    let out = Command::new(bin)
        .args(["archive", "--format=tar", "HEAD"])
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{bin} archive failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.stdout.is_empty(), "{bin} archive produced no output");
    out.stdout
}

/// The `mode` octal field of the first regular-file (`typeflag '0'`) ustar
/// header in a tar stream. Header at 512-byte boundaries; mode is the 8-byte
/// field at offset 100, typeflag the byte at 156.
fn first_file_mode(tar: &[u8]) -> u32 {
    let mut off = 0;
    while off + 512 <= tar.len() {
        let block = &tar[off..off + 512];
        // Skip the all-zero trailer records.
        if block.iter().all(|&b| b == 0) {
            break;
        }
        if block[156] == b'0' {
            let field = &block[100..108];
            let s = std::str::from_utf8(field).unwrap().trim_matches(|c| c == '\0' || c == ' ');
            return u32::from_str_radix(s, 8).unwrap();
        }
        off += 512;
    }
    panic!("no regular-file header found in tar stream");
}

#[test]
fn tar_umask_numeric_matches_git_byte_for_byte() {
    let (repo, home) = fixture("numeric");

    // git forces regular files to a 0666 base before masking, so a 0644 blob
    // archives as 0666 & ~umask. Default (key unset) is git's 0002 → 0664.
    let unset = archive_tar(BIN, &repo, &home);
    assert_eq!(archive_tar("git", &repo, &home), unset, "default archive differs from git");
    assert_eq!(first_file_mode(&unset), 0o664, "default (0002) should be 0666 & ~0002 = 0664");

    // Each numeric mask must reproduce git's bytes exactly, and each must mask
    // the 0666 base git uses — 0027 drops group-write + all of other, 0077 drops
    // the whole group/other triad.
    for (mask, want_mode) in [("0002", 0o664u32), ("0027", 0o640), ("0077", 0o600)] {
        git(&repo, &["config", "tar.umask", mask]);
        let zv = archive_tar(BIN, &repo, &home);
        let real = archive_tar("git", &repo, &home);
        assert_eq!(zv, real, "tar.umask={mask}: zvcs archive differs from git byte-for-byte");
        assert_eq!(first_file_mode(&zv), want_mode, "tar.umask={mask}: wrong masked mode");
    }

    // Distinct masks must yield distinct archives — guards against a broken
    // implementation passing by emitting two equal (e.g. empty) streams.
    git(&repo, &["config", "tar.umask", "0002"]);
    let loose = archive_tar(BIN, &repo, &home);
    git(&repo, &["config", "tar.umask", "0077"]);
    let tight = archive_tar(BIN, &repo, &home);
    assert_ne!(loose, tight, "0002 and 0077 archives must differ");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn tar_umask_user_reads_process_umask_matches_git() {
    let (repo, home) = fixture("user");
    git(&repo, &["config", "tar.umask", "user"]);
    // git resolves `user` by reading the process umask; running both binaries in
    // the same process-umask environment must yield identical bytes.
    let zv = archive_tar(BIN, &repo, &home);
    let real = archive_tar("git", &repo, &home);
    assert_eq!(zv, real, "tar.umask=user: zvcs archive differs from git byte-for-byte");
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
