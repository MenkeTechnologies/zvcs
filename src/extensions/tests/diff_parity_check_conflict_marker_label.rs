//! `diff --check`'s "leftover conflict marker" test is `is_conflict_marker_line()`
//! since git 2.56 (diff.c now calls the merge-ll.c:471-501 helper `add
//! --resolved` shares). It adds one rule to the 2.55 `is_conflict_marker()`: a
//! run of `<` or `>` counts only when a space follows it, since those are the two
//! markers that carry a label. A tab after `<<<<<<<`, or a bare `>>>>>>>` line,
//! is no longer reported; `=======` and `|||||||` still take any whitespace.
//!
//! Expectations measured against stock git 2.56.0.
#![cfg(unix)]

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

#[test]
fn angle_markers_need_a_space_after_them() {
    let dir = std::env::temp_dir().join(format!("zvcs-diffcheck-label-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("f"), "a\n<<<<<<<\tx\n>>>>>>>\n=======\n<<<<<<< ok\n|||||||\t\n").unwrap();

    let out = Command::new(BIN)
        .args(["diff", "--no-index", "--check", "/dev/null", "f"])
        .current_dir(&dir)
        .env("HOME", &dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "f:4: leftover conflict marker\n\
         f:5: leftover conflict marker\n\
         f:6: leftover conflict marker\n\
         f:6: trailing whitespace.\n\
         +|||||||\t\n"
    );
    assert_eq!(out.status.code(), Some(3));
}
