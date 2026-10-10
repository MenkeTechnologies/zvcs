//! `read_repository_format()` runs `read_worktree_config()` over the repository config (and
//! `config.worktree` under `extensions.worktreeConfig`), whose `git_config_bool()` dies on a
//! `core.bare` that is not a boolean: `fatal: bad boolean config value '<v>' for 'core.bare'`,
//! at 128, from setup — ahead of any usage error a builtin parses and for a lone `-h` too.
//! `submodule--helper` parsed its options first and answered with usage at 129.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

/// A repository whose `.git/config` (and `.git/config.worktree`) are the given texts.
fn fixture(bin: &str, tag: &str, config_tail: &str, worktree_config: Option<&str>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zvcs-corebare-{tag}-{}-{}",
        std::process::id(),
        if bin == BIN { "zvcs" } else { "stock" }
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    run(bin, &dir, &["init", "-q", "-b", "main", "."]);
    let config = dir.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(config_tail);
    std::fs::write(&config, text).unwrap();
    if let Some(body) = worktree_config {
        std::fs::write(dir.join(".git/config.worktree"), body).unwrap();
    }
    dir
}

#[test]
fn an_unparsable_core_bare_dies_in_setup_for_every_builtin() {
    let Some(stock) = stock_git() else { return };
    let cases: &[(&str, &str, Option<&str>)] = &[
        ("worktree", "[extensions]\n\tworktreeConfig = true\n", Some("[core]\n\tbare = warn\n")),
        ("quoted", "[extensions]\n\tworktreeConfig = true\n", Some("[core]\n\tbare = \"warn\"\n")),
        ("common", "[core]\n\tbare = sometimes\n", None),
        ("second", "[core]\n\tbare = false\n\tbare = nope\n", None),
        // parsable spellings are not an error
        ("ints", "[core]\n\tbare = 0\n", None),
        ("words", "[core]\n\tbare = off\n", None),
        ("ignored-without-the-extension", "", Some("[core]\n\tbare = warn\n")),
    ];
    let vectors: &[&[&str]] = &[
        &["submodule--helper"],
        &["submodule--helper", "--bogus"],
        &["submodule--helper", "-h"],
        &["status"],
        &["rev-parse", "--git-dir"],
        &["update-ref", "-h"],
    ];
    for (tag, tail, worktree) in cases {
        let (s, z) = (fixture(stock, tag, tail, *worktree), fixture(BIN, tag, tail, *worktree));
        for args in vectors {
            assert_eq!(run(BIN, &z, args), run(stock, &s, args), "{tag}: {args:?}");
        }
        let _ = (std::fs::remove_dir_all(s), std::fs::remove_dir_all(z));
    }
}
