//! `-h` on a superset verb: what it must print, where, and what it must not do.
//!
//! `dispatch.rs` answers `-h`/`--help` for every `z*` verb before dispatch, from
//! the `z_usage` table, and returns. That interception is the whole contract: a
//! verb whose table entry is missing falls through to its own parser instead,
//! which for `zsnapshot` means an argument error and for a verb that reads a
//! leading positional could mean acting on the string `-h`.
//!
//! `zverbs.rs` guards that the table has an entry per verb, which keeps the
//! *listing* complete. This guards the behaviour that entry produces: `-h` exits
//! 0, prints usage on stdout (a help request is output, not a diagnostic — `git
//! zfoo -h | less` has to work), needs no repository, and — asked of the verbs
//! whose purpose is to change things, inside a repository with an untracked
//! file, a staged change and a tag — leaves all of it exactly as it was.
//!
//! Both cases fail if a single verb loses its `z_usage` entry.

use std::path::Path;
use std::process::Command;
use zvcs::dispatch::SUPERSET_VERBS;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap()
}

/// The verbs whose whole purpose is to change something. `-h` must not.
const MUTATORS: &[&str] = &[
    "zclean", "zcommitall", "ztagall", "zcheckout", "zreset", "zrollback", "zrestore", "zrewind",
    "zprune", "zgc", "zpushall", "zundo", "zstash", "zunstash", "zsnapshot", "zbump", "zsync",
];

#[test]
fn every_verb_answers_dash_h_on_stdout_without_a_repository() {
    let root = std::env::temp_dir().join(format!("zvcs-helph-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let empty = root.join("not-a-repo");
    let home = root.join("home");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    for verb in SUPERSET_VERBS {
        let out = run(&empty, &home, &[verb, "-h"]);
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            out.status.success(),
            "`git {verb} -h` exited {:?} outside a repository:\n{stdout}{stderr}",
            out.status.code()
        );
        assert!(
            stdout.to_lowercase().contains("usage"),
            "`git {verb} -h` printed no usage line on stdout (stderr was: {stderr})"
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn asking_a_mutating_verb_for_help_changes_nothing() {
    let root = std::env::temp_dir().join(format!("zvcs-helpm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@example"]);
    run(&repo, &home, &["config", "user.name", "T"]);
    std::fs::write(repo.join("f.txt"), b"v\n").unwrap();
    run(&repo, &home, &["add", "f.txt"]);
    run(&repo, &home, &["commit", "-q", "-m", "c0"]);
    // A worktree with something to lose in every direction: an untracked file,
    // a staged change, and a tag that must not gain a sibling.
    std::fs::write(repo.join("junk.txt"), b"untracked\n").unwrap();
    std::fs::write(repo.join("staged.txt"), b"staged\n").unwrap();
    run(&repo, &home, &["add", "staged.txt"]);
    run(&repo, &home, &["tag", "v1"]);
    run(&repo, &home, &["zreindex", "--sync", root.to_str().unwrap()]);

    let state = |label: &str| -> String {
        let head = String::from_utf8_lossy(&run(&repo, &home, &["rev-parse", "HEAD"]).stdout).trim().to_string();
        let tags = String::from_utf8_lossy(&run(&repo, &home, &["tag"]).stdout).trim().to_string();
        let staged = String::from_utf8_lossy(&run(&repo, &home, &["diff", "--cached", "--name-only"]).stdout)
            .trim()
            .to_string();
        let untracked = repo.join("junk.txt").exists();
        format!("{label}: head={head} tags={tags} staged={staged} untracked={untracked}")
    };
    let before = state("before").replace("before", "state");

    for verb in MUTATORS {
        let out = run(&repo, &home, &[verb, "-h"]);
        assert!(out.status.success(), "`git {verb} -h` failed");
    }

    let after = state("after").replace("after", "state");
    assert_eq!(before, after, "asking for help changed the repository");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_mutator_list_is_made_of_real_verbs() {
    // A typo here would silently shrink the case above to nothing.
    for verb in MUTATORS {
        assert!(SUPERSET_VERBS.contains(verb), "`{verb}` is not a dispatched superset verb");
    }
}
