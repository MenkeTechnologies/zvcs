//! A `.git` directory whose own config says `core.bare = true`, found by walking
//! up from a subdirectory.
//!
//! `repo_discover_implicit_gitdir()` takes its bare arm there (setup.c:1260-1265):
//! `repo_discovery_set_gitdir(discovery, gitdir, offset != cwd->len)` — the git
//! directory is set, realpath'd once the walk climbed, and exported as
//! `$GIT_DIR`, so `--git-dir` prints that string rather than its `.git` /
//! `<cwd>/.git` fallback. zvcs answered `.git` from the subdirectory.
//!
//! Expectations measured from stock git 2.56.0: `.git` at the top, the
//! realpath of the git directory from below, for `--git-dir`, `--git-common-dir`
//! and `--git-path` alike.

use std::path::PathBuf;
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

fn run(root: &PathBuf, cwd: &PathBuf, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

#[test]
fn git_dir_is_the_realpath_below_the_top_and_dot_git_at_it() {
    let root = std::env::temp_dir()
        .join(format!("zvcs-rp-bare-config-gitdir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("w");
    std::fs::create_dir_all(work.join("src/deep")).unwrap();
    let f = Fixture { root };
    assert_eq!(run(&f.root, &work, &["init", "-q", "-b", "main"]).2, 0);
    assert_eq!(run(&f.root, &work, &["config", "core.bare", "true"]).2, 0);
    let real = std::fs::canonicalize(work.join(".git")).unwrap();
    let real = real.to_str().unwrap();

    let args = [
        "rev-parse",
        "--is-bare-repository",
        "--git-dir",
        "--git-common-dir",
        "--is-inside-work-tree",
        "--show-prefix",
        "--git-path",
        "HEAD",
    ];
    assert_eq!(
        run(&f.root, &work, &args),
        ("true\n.git\n.git\nfalse\n\n.git/HEAD\n".to_string(), String::new(), 0)
    );
    for sub in ["src", "src/deep"] {
        assert_eq!(
            run(&f.root, &work.join(sub), &args),
            (format!("true\n{real}\n{real}\nfalse\n\n{real}/HEAD\n"), String::new(), 0),
            "{sub}"
        );
    }
}
