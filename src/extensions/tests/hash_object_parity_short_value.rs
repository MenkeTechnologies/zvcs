//! `hash-object -t` with nothing after it.
//!
//! `get_arg()` (parse-options.c:52-62) ends in
//! `return error(_("%s requires a value"), optname(opt, flags));` — a
//! `PARSE_OPT_ERROR`, which `parse_options()` turns into exit 129 with that one
//! line and no usage block. zvcs printed the usage block after it, the shape of
//! an unknown option.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-hash-object-t-value-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "."]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .stdin(Stdio::null())
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn bad_abbrev(&self, args: &[&str]) -> (String, String, i32) {
        let mut all = vec!["-c", "core.abbrev=bogus", "hash-object"];
        all.extend_from_slice(args);
        self.run(&all)
    }
}

#[test]
fn a_missing_type_is_one_line() {
    let f = Fixture::new("t");
    let want = (String::new(), "error: switch `t' requires a value\n".to_string(), 129);
    assert_eq!(f.run(&["hash-object", "-t"]), want);
    assert_eq!(f.run(&["hash-object", "-wt"]), want);
    assert_eq!(f.bad_abbrev(&["-t"]), want);
}
