//! `git var -l` — the configuration half is `git_config(show_config, NULL)`
//! (builtin/var.c:89), and `show_config()` prints `var=value` or, for a name
//! written with no `=` (a `NULL` value), the bare `var` (builtin/var.c:73-80).

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-var-list-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join(".xdg"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("GIT_CONFIG_SYSTEM")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

/// A valueless name lists bare, an empty value keeps its `=`, and the
/// distinction holds for every occurrence of a repeated name, not only the last.
#[test]
fn valueless_names_list_without_equals() {
    let dir = fixture("valueless");
    git(&dir, &["init", "-q", "repo"]);
    let repo = dir.join("repo");
    let config = repo.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("[x]\n\ta\n\ta = 1\n\ta\n\tb\n\tb =\n");
    std::fs::write(&config, text).unwrap();

    let listed = git(&repo, &["var", "-l"]);
    let section: Vec<&str> = listed.lines().filter(|l| l.starts_with("x.")).collect();
    assert_eq!(section, ["x.a", "x.a=1", "x.a", "x.b", "x.b="]);

    let _ = std::fs::remove_dir_all(&dir);
}
