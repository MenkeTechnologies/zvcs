//! `git stash import` without exactly one operand.
//!
//! `import_stash()` runs `parse_options()` (flags 0, so a `--` is consumed) and then
//! `if (argc != 1) usage_msg_opt(_("a revision is required"), ...)`: `fatal: a revision is
//! required`, a blank line, the usage block, status 129 — whether the operand is missing or
//! there is more than one. zvcs answered `` `stash import` is not ported `` (128).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

#[test]
fn import_wants_exactly_one_operand() {
    let Some(stock) = stock_git() else { return };
    let dir = std::env::temp_dir().join(format!("zvcs-stashimport-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let run = |bin: &str, args: &[&str]| {
        let out = Command::new(bin)
            .args(args)
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &dir)
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code(),
        )
    };
    assert!(Command::new(stock).args(["init", "-q"]).current_dir(&dir).status().unwrap().success());
    for args in [
        &["stash", "import"][..],
        &["stash", "import", "a", "b"],
        &["stash", "import", "--", "a", "b"],
        &["stash", "import", "a", "--"],
        &["stash", "import", "-h"],
        &["stash", "import", "--bogus"],
    ] {
        assert_eq!(run(BIN, args), run(stock, args), "args {args:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
