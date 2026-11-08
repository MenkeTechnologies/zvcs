//! A files transaction refuses a name conflict whoever runs it, as
//! `lock_raw_ref()` (refs/files-backend.c:795-829, :903-934) and the check of
//! the refs created loose against `packed-refs` (:3064-3082) do: receive-pack's
//! `--atomic` transaction reports the store's text and fails every command
//! (builtin/receive-pack.c:2010-2014), with the conflicting ref loose or packed.
//!
//! A ref someone else holds locked is reported by `unable_to_lock_message()`
//! (lockfile.c:254-299) with the path `files_ref_path()` builds from the git
//! directory `enter_repo()` left at `.` (setup.c:1889-1897), so it reads
//! `<git-dir>/./refs/heads/<name>.lock`, in a bare and a non-bare repository,
//! with and without `--atomic`.
//!
//! Each case builds identical fixtures with stock git, pushes with stock git and
//! swaps only the receiving end, on the files and the reftable backend.

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::Path;
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// Run `script` under `sh` in a fresh `root`, with `$S` the stock git that builds the
/// fixture and `$G` the git under test, and report its output with `root` spelled `<D>`.
fn run(stock: &str, git: &str, root: &Path, format: &str, script: &str) -> String {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).unwrap();
    let out = Command::new("sh")
        .args(["-c", script])
        .current_dir(root)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "maintenance.auto")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("COLUMNS", "80")
        .env("LC_ALL", "C")
        .env("S", stock)
        .env("G", git)
        .env("F", format)
        .output()
        .unwrap();
    let text = format!(
        "{}--- stderr\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    text.replace(&root.display().to_string(), "<D>")
}

fn compare(name: &str, script: &str) {
    let Some(stock) = stock_git() else { return };
    for format in ["files", "reftable"] {
        let root = std::env::temp_dir().join(format!("zvcs-receive-name-lock-{name}-{format}-{}", std::process::id()));
        let want = run(stock, stock, &root, format, script);
        let got = run(stock, ZVCS, &root, format, script);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(got, want, "{name} ({format})");
    }
}

/// `bare.git` (or the non-bare `nb`) holds `main`, `side` packed and `loose`
/// loose; `w` has `main`, `side/x`, `loose/y` and `other` one commit ahead.
const FIXTURE: &str = r#"
$S init -q -b main --ref-format=$F --bare bare.git
$S init -q -b main --ref-format=$F nb; $S -C nb commit -q --allow-empty -m z; $S -C nb checkout -q --detach
$S -C nb config receive.denyCurrentBranch ignore
$S init -q -b main --ref-format=$F w; cd w
$S commit -q --allow-empty -m one
$S push -q ../bare.git main main:side main:loose; $S push -q ../nb main main:side main:loose
$S -C ../bare.git pack-refs --all; $S -C ../bare.git update-ref refs/heads/loose refs/heads/main
$S commit -q --allow-empty -m two
$S branch side/x; $S branch loose/y; $S branch other; $S branch -f side
show() { echo "exit $?"; $S -C $1 for-each-ref; }
"#;

#[test]
fn an_atomic_push_with_a_name_conflict_fails_every_command() {
    for spec in ["side/x other", "loose/y other"] {
        compare(
            &format!("atomic-{}", spec.replace(['/', ' '], "-")),
            &format!("{FIXTURE}$S push --atomic --receive-pack=\"$G receive-pack\" ../bare.git {spec}; show ../bare.git"),
        );
    }
}

#[test]
fn a_held_lock_names_the_path_from_the_dot_git_dir() {
    for atomic in ["--atomic", "--no-atomic"] {
        for (repo, dir) in [("bare.git", "bare.git"), ("nb", "nb/.git")] {
            compare(
                &format!("lock{atomic}-{repo}"),
                &format!(
                    "{FIXTURE}[ $F = files ] && touch ../{dir}/refs/heads/side.lock ../{dir}/refs/heads/new.lock\n\
                     $S push {atomic} --receive-pack=\"$G receive-pack\" ../{repo} side other main:new; show ../{repo}"
                ),
            );
        }
    }
}
