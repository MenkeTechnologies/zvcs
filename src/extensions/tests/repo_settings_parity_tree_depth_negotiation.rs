//! Two `prepare_repo_settings()` reads that never refused anything.
//!
//! `repo_cfg_int(r, "core.maxtreedepth", …)` (repo-settings.c:103-105) dies
//! through `git_config_int()` on a value it cannot parse, and
//! `fetch.negotiationalgorithm` (repo-settings.c:123-138) is matched
//! case-insensitively against `skipping`, `noop`, `consecutive` and `default`,
//! with anything else
//!
//! ```c
//! die("unknown fetch negotiation algorithm '%s'", strval);
//! ```
//!
//! Both run for every command that prepares the settings — `rev-parse`,
//! `log`, `status` — not only `fetch`. zvcs validated neither in the settings
//! block, so `log` and `rev-parse` ran, and `pull` reported the algorithm from
//! `fetch`'s own read at exit 1.
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
            .join(format!("zvcs-repo-settings-depth-negotiation-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["branch", "side"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

const NEGOTIATION: &str = "fatal: unknown fetch negotiation algorithm 'bogus'\n";

#[test]
fn an_unknown_negotiation_algorithm_stops_every_settings_verb() {
    let f = Fixture::new("negotiation");
    for verb in [
        &["rev-parse", "--git-dir"][..],
        &["log", "-1", "--oneline"],
        &["status", "--porcelain"],
        &["fetch", "."],
        &["pull", ".", "side"],
    ] {
        let mut args = vec!["-c", "fetch.negotiationAlgorithm=bogus"];
        args.extend_from_slice(verb);
        assert_eq!(f.run(&args), (String::new(), NEGOTIATION.to_owned(), 128), "{verb:?}");
    }
}

#[test]
fn the_known_algorithms_pass_in_any_case() {
    let f = Fixture::new("known");
    for value in ["skipping", "NOOP", "Consecutive", "default"] {
        let (out, err, code) = f.run(&[
            "-c",
            &format!("fetch.negotiationAlgorithm={value}"),
            "rev-parse",
            "--git-dir",
        ]);
        assert_eq!((out.as_str(), err.as_str(), code), (".git\n", "", 0), "{value}");
    }
}

#[test]
fn a_bad_max_tree_depth_is_a_numeric_refusal() {
    let f = Fixture::new("depth");
    assert_eq!(
        f.run(&["-c", "core.maxTreeDepth=bogus", "log", "-1", "--oneline"]),
        (
            String::new(),
            "fatal: bad numeric config value 'bogus' for 'core.maxtreedepth': invalid unit\n"
                .to_owned(),
            128
        )
    );
    // A valueless key reads as the empty string, which is no number either.
    assert_eq!(
        f.run(&["-c", "core.maxTreeDepth", "rev-parse", "--git-dir"]),
        (
            String::new(),
            "fatal: bad numeric config value '' for 'core.maxtreedepth': invalid unit\n".to_owned(),
            128
        )
    );
    let (out, _, code) = f.run(&["-c", "core.maxTreeDepth=5", "rev-parse", "--git-dir"]);
    assert_eq!((out.as_str(), code), (".git\n", 0));
}

/// The settings block reads in C order: `core.maxtreedepth` (repo-settings.c:103)
/// before `fetch.negotiationalgorithm` (:123), and both before
/// `core.packedgitlimit` (:155).
#[test]
fn the_refusals_are_reported_in_read_order() {
    let f = Fixture::new("order");
    let (_, err, _) = f.run(&[
        "-c",
        "fetch.negotiationAlgorithm=bogus",
        "-c",
        "core.maxTreeDepth=bogus",
        "rev-parse",
        "--git-dir",
    ]);
    assert_eq!(
        err,
        "fatal: bad numeric config value 'bogus' for 'core.maxtreedepth': invalid unit\n"
    );
    let (_, err, _) = f.run(&[
        "-c",
        "core.packedGitLimit=bogus",
        "-c",
        "fetch.negotiationAlgorithm=bogus",
        "rev-parse",
        "--git-dir",
    ]);
    assert_eq!(err, NEGOTIATION);
}
