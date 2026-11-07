//! `count-objects -v` reports garbage in the order `readdir()` hands the
//! entries over, not sorted: `for_each_file_in_obj_subdir()`
//! (object-file.c:1062-1074) and `for_each_file_in_pack_subdir()`
//! (packfile.c:828-843) walk the directory as it comes, and only the files
//! with a pack suffix are sorted, by `report_pack_garbage()`. zvcs sorted every
//! directory first, so on a filesystem whose order is not alphabetical the
//! warnings came out in a different order.
//!
//! Both binaries run against the *same* repository — `count-objects` writes
//! nothing — so they see one directory order, whatever the filesystem's is.

use std::path::Path;
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

fn git(bin: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    (
        out.status.code().expect("no signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn garbage_comes_out_in_directory_order() {
    let Some(stock) = stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-count-objects-readdir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let run = |args: &[&str]| assert_eq!(git(stock, &dir, args).0, 0, "{args:?}");
    run(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a"), "a\n").unwrap();
    run(&["add", "a"]);
    run(&["commit", "-q", "-m", "a"]);
    run(&["repack", "-q", "-a", "-d"]);
    std::fs::write(dir.join("a"), "b\n").unwrap();
    run(&["commit", "-q", "-am", "b"]);

    // Enough names, created out of alphabetical order, that a sorted walk and
    // the filesystem's own order disagree somewhere.
    let objects = dir.join(".git/objects");
    let pack = objects.join("pack");
    let fan = objects.join("ab");
    std::fs::create_dir_all(&fan).unwrap();
    for name in ["zeta", "tmp_pack_q", "garbage.txt", "mid", "a-first", "tmp_idx_r", "omega.tmp"] {
        std::fs::write(pack.join(name), "junk\n").unwrap();
    }
    for name in ["notanobject", "cdef.tmp", "tmp_obj_xyz", "zz", "b", "0123"] {
        std::fs::write(fan.join(name), "junk\n").unwrap();
    }

    for args in [&["count-objects", "-v"][..], &["count-objects", "-vH"]] {
        let want = git(stock, &dir, args);
        let got = git(ZVCS, &dir, args);
        assert_eq!(want.0, 0, "{args:?}: {want:?}");
        assert_eq!(got, want, "{args:?}: left is zvcs, right is stock");
    }
    let _ = std::fs::remove_dir_all(&root);
}
