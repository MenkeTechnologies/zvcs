//! The DOS date/time pair `git archive --format=zip` stamps into every header.
//!
//! `archive-zip.c:613-628` derives that pair with `localtime_r()`, not
//! `gmtime_r()`, so the two bytes move with the process's `TZ` while the
//! `UT` extra field beside them keeps the raw epoch seconds. A port that
//! computed the pair in UTC produced a zip that is byte-identical to stock's
//! only in `TZ=UTC` and differs in every other zone — invisible to any test
//! that unzips the archive, because `unzip` prefers the `UT` field.
//!
//! Every expectation below was measured from stock git 2.55.0 over a commit at
//! `1700000000 +0000`, reading the little-endian `u16` pair at offset 10 of the
//! first local file header:
//!
//! ```text
//! $ TZ=UTC0  git archive --format=zip HEAD | head -c 14 | xxd
//! time=0xb1aa date=0x576e   # 22:13:20 2023-11-14
//! $ TZ=XXX-5 git archive --format=zip HEAD | head -c 14 | xxd
//! time=0x19aa date=0x576f   #  3:13:20 2023-11-15
//! $ TZ=EST5  git archive --format=zip HEAD | head -c 14 | xxd
//! time=0x89aa date=0x576e   # 17:13:20 2023-11-14
//! ```
//!
//! The three zones are POSIX `TZ` strings with no DST rule, so they resolve the
//! same way on any libc without needing a tz database entry.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A one-file repository whose only commit is at `1700000000 +0000`.
fn fixture() -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zip-dostime-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = repo.canonicalize().unwrap();
    let home = home.canonicalize().unwrap();
    run(&repo, &home, "UTC0", &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f.txt"), "hello").unwrap();
    run(&repo, &home, "UTC0", &["add", "f.txt"]);
    run(&repo, &home, "UTC0", &["commit", "-q", "-m", "c1"]);
    (root, repo, home)
}

fn run(dir: &Path, home: &Path, tz: &str, args: &[&str]) -> Vec<u8> {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("TZ", tz)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run binary");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// The `(dos_time, dos_date)` pair at offset 10 of a local file header.
fn dos_pair(zip: &[u8]) -> (u16, u16) {
    assert_eq!(&zip[..4], b"PK\x03\x04", "not a local file header");
    (
        u16::from_le_bytes([zip[10], zip[11]]),
        u16::from_le_bytes([zip[12], zip[13]]),
    )
}

/// The `UT` extended-timestamp extra, which holds the epoch seconds and so does
/// *not* move with `TZ`. Its presence is what makes the DOS pair the only
/// TZ-sensitive field, and why an unzip listing hides the bug.
fn ut_seconds(zip: &[u8]) -> u32 {
    let name_len = u16::from_le_bytes([zip[26], zip[27]]) as usize;
    let extra_at = 30 + name_len;
    assert_eq!(&zip[extra_at..extra_at + 2], b"UT", "no extended-timestamp extra");
    u32::from_le_bytes([
        zip[extra_at + 5],
        zip[extra_at + 6],
        zip[extra_at + 7],
        zip[extra_at + 8],
    ])
}

#[test]
fn zip_dos_stamp_follows_the_process_timezone() {
    let (root, repo, home) = fixture();

    for (tz, time, date) in
        [("UTC0", 0xb1aau16, 0x576eu16), ("XXX-5", 0x19aa, 0x576f), ("EST5", 0x89aa, 0x576e)]
    {
        let zip = run(&repo, &home, tz, &["archive", "--format=zip", "HEAD"]);
        assert_eq!(
            dos_pair(&zip),
            (time, date),
            "TZ={tz}: DOS pair is not localtime_r()'s; got \
             {:02}:{:02}:{:02} on {}-{:02}-{:02}",
            dos_pair(&zip).0 >> 11,
            (dos_pair(&zip).0 >> 5) & 0x3f,
            (dos_pair(&zip).0 & 0x1f) * 2,
            1980 + (dos_pair(&zip).1 >> 9),
            (dos_pair(&zip).1 >> 5) & 0xf,
            dos_pair(&zip).1 & 0x1f,
        );
        // The commit's own instant, unchanged by the zone: 1700000000.
        assert_eq!(ut_seconds(&zip), 1_700_000_000, "TZ={tz}: UT extra moved");
    }

    // Two zones five hours apart must not produce the same bytes: this is the
    // assertion a gmtime-based pair fails, since it is constant across zones.
    let utc = run(&repo, &home, "UTC0", &["archive", "--format=zip", "HEAD"]);
    let east = run(&repo, &home, "XXX-5", &["archive", "--format=zip", "HEAD"]);
    assert_ne!(utc, east, "zip bytes did not move with TZ at all");

    let _ = std::fs::remove_dir_all(&root);
}
