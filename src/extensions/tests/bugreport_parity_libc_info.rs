//! `git bugreport` — the `libc info:` line of `[System Info]`.
//!
//! `get_libc_info()` (compat/compiler.h:28-38) prints `glibc: <version>` from
//! `gnu_get_libc_version()` whenever the binary was built against glibc, and
//! `no libc information available` otherwise. Stock git 2.55.0 on macOS prints
//! the fallback; on a glibc Linux host it prints the loaded glibc, which a
//! `*-linux-gnu` Rust binary links just the same. The expected value is read
//! from `getconf GNU_LIBC_VERSION` — a source independent of this binary — so
//! the test cannot pass by agreeing with itself.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-bugreport-libc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    /// The `libc info:` line of a `--no-suffix` report written outside any
    /// repository, with the editor pinned to the `:` no-op.
    fn libc_line(&self) -> String {
        let out = Command::new(BIN)
            .args(["bugreport", "--no-suffix"])
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_EDITOR", ":")
            .output()
            .unwrap();
        assert!(out.status.success(), "bugreport failed: {out:?}");
        let text = std::fs::read_to_string(self.root.join("git-bugreport.txt")).unwrap();
        text.lines()
            .find(|l| l.starts_with("libc info: "))
            .unwrap_or_else(|| panic!("no libc info line:\n{text}"))
            .to_string()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
fn glibc_build_reports_the_loaded_glibc() {
    // `getconf GNU_LIBC_VERSION` prints `glibc 2.39`.
    let conf = Command::new("getconf").arg("GNU_LIBC_VERSION").output().unwrap();
    let conf = String::from_utf8(conf.stdout).unwrap();
    let version = conf.trim().strip_prefix("glibc ").expect("getconf GNU_LIBC_VERSION");
    assert_eq!(Fixture::new().libc_line(), format!("libc info: glibc: {version}"));
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
#[test]
fn non_glibc_build_takes_gits_fallback() {
    assert_eq!(Fixture::new().libc_line(), "libc info: no libc information available");
}
