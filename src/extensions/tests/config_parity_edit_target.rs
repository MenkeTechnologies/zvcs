//! `git config --edit` is `show_editor()` (builtin/config.c:1291-1323): only
//! `--global` creates its file, `O_CREAT | O_EXCL`, seeded with
//! `default_user_config()` (builtin/config.c:915-927, the ident lines taken from
//! `ident_default_name()`/`ident_default_email()`); any other target goes to
//! `launch_editor()` as it is, whose `strbuf_realpath(..., 1)` dies
//! `Invalid path '<dir>': <strerror>` for a file in a missing directory
//! (editor.c:88). zvcs created every target and reported a missing directory as a
//! `zvcs:` I/O error.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, home: &Path, editor: &str, extra: &[(&str, &str)], args: &[&str]) -> (String, String, i32) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", editor)
        .env("LC_ALL", "C");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

#[test]
fn config_edit_matches_stock() {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-config-edit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let cases: [(&str, &[(&str, &str)], &[&str]); 6] = [
        ("true", &[], &["config", "--edit", "--file=/nonexistent-zvcs/x"]),
        (":", &[], &["config", "--edit", "--file=/nonexistent-zvcs/x"]),
        ("true", &[], &["config", "--edit", "--file=fresh"]),
        ("true", &[], &["config", "--global", "--edit"]),
        ("true", &[], &["-c", "user.email=", "-c", "user.name=N", "config", "edit", "--global"]),
        ("true", &[("EMAIL", "env@example.com")], &["-c", "author.name=X", "config", "--global", "--edit"]),
    ];
    for (i, (editor, extra, args)) in cases.iter().enumerate() {
        let mut seen = Vec::new();
        for (tag, bin) in [("z", BIN), ("s", stock)] {
            let dir = root.join(format!("{tag}{i}"));
            let home = root.join(format!("{tag}{i}.home"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::create_dir_all(&home).unwrap();
            assert_eq!(run(stock, &dir, &home, "true", &[], &["init", "-q", "."]).2, 0);
            let out = run(bin, &dir, &home, editor, extra, args);
            let global = std::fs::read_to_string(home.join(".gitconfig")).ok();
            let fresh = dir.join("fresh").exists();
            seen.push((out, global, fresh));
        }
        assert_eq!(seen[0], seen[1], "{editor} {args:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
