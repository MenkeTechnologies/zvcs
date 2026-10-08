//! `git bisect start` with no revision, against stock git.
//!
//! `bisect_start()` resolves `HEAD` before it writes anything, and every revision-name lookup
//! prepares the repository settings — so a `core.*` value git cannot read ends the command with
//! `fatal:` at 128, ahead of the term checks and with no `BISECT_*` file left behind. The
//! option scan and the `--reset-when-found`/`--no-checkout` check still come first.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

fn session_files(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join(".git"))
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("BISECT_"))
        .collect();
    names.sort();
    names
}

#[test]
fn a_start_without_revisions_builds_the_settings() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let base = std::env::temp_dir().join(format!("zvcs-bisect-start-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let keys = [
        "core.commitGraph=zz",
        "core.packedGitLimit=zz",
        "commitGraph.generationVersion=zz",
        "index.version=zz",
        "core.useReplaceRefs=zz",
        // Read by neither the settings block nor the default callback.
        "core.fsyncMethod=zz",
    ];
    let tails: [&[&str]; 4] = [
        &["bisect", "start"],
        // The term check sits behind the settings ...
        &["bisect", "start", "--term-good=a", "--term-bad=a"],
        // ... and the option scan and the `--no-checkout` conflict sit in front of them.
        &["bisect", "start", "--bogus"],
        &["bisect", "start", "--reset-when-found", "--no-checkout"],
    ];
    for key in keys {
        for tail in tails {
            let mut args = vec!["-c", key];
            args.extend_from_slice(tail);
            let mut seen = Vec::new();
            for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
                let root = base.join(who);
                let _ = std::fs::remove_dir_all(&root);
                std::fs::create_dir_all(&root).unwrap();
                run(stock, &root, &["init", "-q", "-b", "main"]);
                std::fs::write(root.join("f"), "a\n").unwrap();
                run(stock, &root, &["add", "f"]);
                run(stock, &root, &["commit", "-qm", "one"]);
                seen.push((run(bin, &root, &args), session_files(&root)));
            }
            assert_eq!(seen[1], seen[0], "{args:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}
