//! Clumped short options, and `-d`, for the `git worktree` subcommands.
//!
//! Every subcommand runs `parse_options()` over its own table, whose
//! `parse_short_opt()` consumes a `-xyz` word one character at a time and lets a
//! value-taking option swallow the rest of the word. The short options are
//! `prune`'s `-n`/`-v` (builtin/worktree.c:252-253), `add`'s `-f`, `-b`, `-B`,
//! `-d` and `-q` (worktree.c:805-818 — `OPT_BOOL('d', "detach", …)` at :813),
//! `list`'s `-v`/`-z` (:1089-1092), and `-f` for `move` (:1250) and `remove`
//! (:1383). zvcs matched whole tokens only, so `add -qd`, `prune -nv`,
//! `remove -ff` and `move -ff` were "unknown switch", and `add -d` was refused
//! outright.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-worktree-clumps-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("repo");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "one"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn head_of(&self, id: &str) -> String {
        std::fs::read_to_string(self.work.join(".git/worktrees").join(id).join("HEAD")).unwrap()
    }
}

#[test]
fn add_takes_d_and_clumps() {
    let f = Fixture::new("add");
    let (out, err, code) = f.run(&["worktree", "add", "-d", "../d1"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("HEAD is now at 699945f one\n", "Preparing worktree (detached HEAD 699945f)\n", 0)
    );
    assert!(!f.head_of("d1").starts_with("ref:"));

    // `-q` then `-d`: silent and detached.
    assert_eq!(f.run(&["worktree", "add", "-qd", "../d2"]), (String::new(), String::new(), 0));
    assert!(!f.head_of("d2").starts_with("ref:"));

    // `-b` swallows the rest of its word.
    assert_eq!(f.run(&["worktree", "add", "-qbnb", "../b1"]), (String::new(), String::new(), 0));
    assert_eq!(f.head_of("b1"), "ref: refs/heads/nb\n");

    // A clump that ends on `-b` takes the next word, and one that runs out does not.
    let (out, err, code) = f.run(&["worktree", "add", "-qb"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "error: switch `b' requires a value\n", 129)
    );
    // An unknown character is named on its own.
    let (_, err, code) = f.run(&["worktree", "add", "-qx", "../z"]);
    assert!(err.starts_with("error: unknown switch `x'\nusage: git worktree add "), "{err}");
    assert_eq!(code, 129);
    assert!(!f.root.join("z").exists());
}

#[test]
fn list_prune_remove_and_move_split_their_clumps() {
    let f = Fixture::new("others");
    assert_eq!(f.run(&["worktree", "add", "-q", "--detach", "../gone"]).2, 0);
    std::fs::remove_dir_all(f.root.join("gone")).unwrap();

    // `-v` and `-z` both parsed, so the `-z` check fires rather than a usage block.
    let (out, err, code) = f.run(&["worktree", "list", "-vz"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: the option '-z' requires '--porcelain'\n", 128)
    );

    let (out, err, code) = f.run(&["worktree", "prune", "-nv"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "Removing worktrees/gone: gitdir file points to non-existent location\n", 0)
    );
    assert!(f.work.join(".git/worktrees/gone").exists(), "-n must not prune");

    assert_eq!(
        f.run(&["worktree", "add", "-q", "--detach", "--lock", "../m1"]).2,
        0
    );
    assert_eq!(f.run(&["worktree", "move", "-ff", "../m1", "../m2"]), (String::new(), String::new(), 0));
    assert!(f.root.join("m2/.git").exists());
    assert_eq!(f.run(&["worktree", "remove", "-ff", "../m2"]), (String::new(), String::new(), 0));
    assert!(!f.root.join("m2").exists());
    assert!(!f.work.join(".git/worktrees/m1").exists());
}

/// A requested `-h` exits 0 since git 2.56 (parse-options.c:1207-1208,
/// PARSE_OPT_HELP); 2.55 exited 129.
#[test]
fn a_help_clump_prints_the_subcommand_usage() {
    let f = Fixture::new("help");
    let (out, err, code) = f.run(&["worktree", "prune", "-nh"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "usage: git worktree prune [-n] [-v] [--expire <expire>]\n\
             \n    -n, --[no-]dry-run    do not remove, show only\n\
             \x20   -v, --[no-]verbose    report pruned working trees\n\
             \x20   --[no-]expire <expiry-date>\n\
             \x20                         prune missing working trees older than <time>\n\n",
            "",
            0
        )
    );
}
