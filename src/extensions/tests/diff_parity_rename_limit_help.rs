//! The `-l <n>` line of the diff options' help says what the limit does since
//! 2.56: past it rename/copy detection is limited to the cheap exact pass, not
//! switched off.
//!
//! diff.c:6167-6168 (v2.56.0) reworded the `OPT_INTEGER('l', …)` help from
//! "prevent rename/copy detection if the number of rename/copy targets exceeds
//! given limit". Every command whose `-h` lists the diff options prints it.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const LINE: &str = "    -l <n>                limit to cheap rename/copy detection if the number of \
                    rename/copy targets exceeds this value\n";

fn help(cmd: &str) -> String {
    let dir = std::env::temp_dir();
    let out = Command::new(BIN)
        .args([cmd, "-h"])
        .current_dir(&dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", &dir)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{cmd} -h");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn the_rename_limit_help_names_the_cheap_pass() {
    for cmd in ["diff", "diff-pairs", "range-diff"] {
        let text = help(cmd);
        assert!(text.contains(LINE), "{cmd} -h:\n{text}");
        assert!(!text.contains("prevent rename/copy detection"), "{cmd} -h:\n{text}");
    }
}
