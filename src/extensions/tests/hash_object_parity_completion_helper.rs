//! `git hash-object --git-completion-helper[-all]` — the option list the bash and
//! zsh completions offer after `git hash-object --<TAB>`.
//!
//! `cmd_hash_object()` calls `parse_options()` before any setup
//! (builtin/hash-object.c:99-115), and `parse_options()` answers a lone
//! completion-helper argument itself (parse-options.c:1057-1060). zvcs vets
//! hash-object's options ahead of its configuration gates and refused both
//! spellings there as unknown options, with the usage block and exit 129.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(arg: &str) -> (String, String, Option<i32>) {
    let dir = std::env::temp_dir().join(format!("zvcs-hash-object-gitcomp-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let out = Command::new(BIN)
        .args(["hash-object", arg])
        .current_dir(&dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

#[test]
fn completion_helper_lists_the_options() {
    let want = " --stdin --stdin-paths --no-filters --literally --path= --filters -- \
                --no-stdin --no-stdin-paths --no-literally --no-path\n";
    for arg in ["--git-completion-helper", "--git-completion-helper-all"] {
        assert_eq!(run(arg), (want.to_string(), String::new(), Some(0)), "{arg}");
    }
}
