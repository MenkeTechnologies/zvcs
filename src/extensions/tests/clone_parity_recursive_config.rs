//! What `clone --recurse-submodules` adds to the configuration it writes.
//!
//! Straight after the banner, `cmd_clone()` turns the recurse pathspecs into
//! `submodule.active` pairs on `option_config` — sorted and deduplicated by
//! `string_list_sort_u()` — adds `submodule.recurse=true` when the ambient
//! `submodule.stickyRecursiveClone` is true, and records how submodules borrow
//! from the superproject's references: `alternateLocation=superproject` with
//! `alternateErrorStrategy=die` for `--reference`, `=info` for
//! `--reference-if-able`, and a `die()` when both were given
//! (builtin/clone.c:1141-1180). A bare `--recurse-submodules` contributes the
//! option's `defval` `.` (`recurse_submodules_cb()`, :83-95), even next to a
//! pathspec. zvcs wrote only `submodule.active`, unsorted and with duplicates,
//! dropped `.` whenever a pathspec was also given, and ignored the rest.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `src` with one commit and no submodules, and `r1`, a plain clone of it
    /// that can serve as a reference.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-clone-recursive-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        let f = Fixture { root };
        let src = f.root.join("src");
        f.run(&src, &[], &["init", "-q", "-b", "main", "."]);
        std::fs::write(src.join("a"), "a\n").unwrap();
        f.run(&src, &[], &["add", "a"]);
        f.run(&src, &[], &["commit", "-q", "-m", "a"]);
        f.run(&f.root, &[], &["clone", "-q", "src", "r1"]);
        f
    }

    /// Clone `src` to `dst` with `opts` (quietly, successfully) and return the
    /// clone's `submodule.*` configuration as `config --get-regexp` prints it.
    fn submodule_config(&self, env: &[(&str, &str)], dst: &str, opts: &[&str]) -> String {
        let mut args = vec!["clone", "-q"];
        args.extend_from_slice(opts);
        args.extend_from_slice(&["src", dst]);
        assert_eq!(self.run(&self.root, env, &args), (String::new(), String::new(), 0), "{opts:?}");
        self.run(&self.root.join(dst), &[], &["config", "--local", "--get-regexp", "^submodule\\."]).0
    }

    fn run(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.root)
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
            .env("TZ", "UTC")
            .envs(env.iter().copied())
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

#[test]
fn references_set_the_submodule_alternate_strategy() {
    let f = Fixture::new("refs");
    assert_eq!(
        f.submodule_config(&[], "d1", &["--recursive", "--reference", "r1"]),
        "submodule.active .\nsubmodule.alternatelocation superproject\nsubmodule.alternateerrorstrategy die\n"
    );
    assert_eq!(
        f.submodule_config(&[], "d2", &["--recurse-submodules", "--reference-if-able", "r1"]),
        "submodule.active .\nsubmodule.alternatelocation superproject\nsubmodule.alternateerrorstrategy info\n"
    );
    // Without a recurse request the references say nothing about submodules.
    assert_eq!(f.submodule_config(&[], "d3", &["--reference", "r1"]), "");
}

#[test]
fn both_reference_kinds_are_refused_after_the_banner() {
    let f = Fixture::new("both");
    let (out, err, code) = f.run(
        &f.root,
        &[],
        &["clone", "--recursive", "--reference", "r1", "--reference-if-able", "r1", "src", "dst"],
    );
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "Cloning into 'dst'...\nfatal: clone --recursive is not compatible with both --reference and --reference-if-able\n",
            128
        )
    );
    assert!(!f.root.join("dst").exists());
}

#[test]
fn pathspecs_are_sorted_deduplicated_and_keep_the_default() {
    let f = Fixture::new("active");
    assert_eq!(
        f.submodule_config(&[], "d", &["--recursive=b", "--recursive", "--recursive=b"]),
        "submodule.active .\nsubmodule.active b\n"
    );
}

#[test]
fn sticky_recursive_clone_records_submodule_recurse() {
    let f = Fixture::new("sticky");
    let sticky = [
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "submodule.stickyRecursiveClone"),
        ("GIT_CONFIG_VALUE_0", "true"),
    ];
    assert_eq!(
        f.submodule_config(&sticky, "d1", &["--recursive"]),
        "submodule.active .\nsubmodule.recurse true\n"
    );

    let bogus = [
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "submodule.stickyRecursiveClone"),
        ("GIT_CONFIG_VALUE_0", "bogus"),
    ];
    let (out, err, code) = f.run(&f.root, &bogus, &["clone", "--recursive", "src", "d2"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "Cloning into 'd2'...\nfatal: bad boolean config value 'bogus' for 'submodule.stickyRecursiveClone'\n",
            128
        )
    );
    assert!(!f.root.join("d2").exists());
}
