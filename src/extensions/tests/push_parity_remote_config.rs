//! remote.c's `read_config()` (remote.c:630-650) walks the configuration through
//! `handle_config()` (remote.c:431-609) the first time a command asks for a
//! remote — `push`, `fetch`, `ls-remote`, `git remote`, and `pull` (from
//! `config_get_rebase()`'s `branch_get("HEAD")`, builtin/pull.c:200, or
//! `get_rebase_fork_point()`). That walk refuses:
//!
//! * `remote.<name>.{mirror,skipDefaultUpdate,skipFetchAll,prune,pruneTags}`
//!   through `git_config_bool()` — `bad boolean config value` (remote.c:505-514);
//! * a valueless `remote.<name>.{url,pushurl,push,fetch,receivepack,…}`,
//!   `remote.pushDefault`, `branch.<name>.{remote,pushremote,merge}` and
//!   `url.<base>.insteadOf` — `missing value`, then the source line;
//! * `branch..<key>`, an empty subsection, with `return -1` and no `error()`.
//!
//! It also warns about a `remote./<x>` section and reports a second
//! `receivepack` as an `error:` without stopping. zvcs never ran the walk, so
//! all of these pushed, fetched and listed as though the value were fine.
//!
//! `pull` spawns `git fetch` as a child: a refusal in the pull itself is a
//! `die()` at 128, while one reached only by the child comes back as exit 1.
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
    /// A bare `up.git` holding `main`, and a work repository `w` whose remote
    /// `o` points at it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-remote-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("w");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run_in(&f.root, &["init", "-q", "--bare", "up.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["remote", "add", "o", "../up.git"]);
        f.run(&["push", "-q", "o", "main"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
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

    fn with(&self, config: &[&str], cmd: &[&str]) -> (String, String, i32) {
        let mut args: Vec<&str> = Vec::new();
        for c in config {
            args.push("-c");
            args.push(c);
        }
        args.extend_from_slice(cmd);
        self.run(&args)
    }
}

const TRANSPORT_VERBS: [&[&str]; 5] = [
    &["push", "o", "main"],
    &["fetch", "o"],
    &["ls-remote", "o"],
    &["remote"],
    &["pull", "o", "main"],
];

#[test]
fn boolean_remote_keys_are_fatal_for_every_transport_verb() {
    let f = Fixture::new("bools");
    for (config, lower) in [
        ("remote.o.prune=bogus", "remote.o.prune"),
        // A remote nobody names is still parsed.
        ("remote.x.pruneTags=bogus", "remote.x.prunetags"),
        ("remote.o.mirror=bogus", "remote.o.mirror"),
        ("remote.o.skipFetchAll=bogus", "remote.o.skipfetchall"),
    ] {
        for cmd in TRANSPORT_VERBS {
            let (out, err, code) = f.with(&[config], cmd);
            let want = format!("fatal: bad boolean config value 'bogus' for '{lower}'\n");
            assert_eq!(
                (out.as_str(), err.as_str(), code),
                ("", want.as_str(), 128),
                "{config} {cmd:?}"
            );
        }
    }
}

#[test]
fn valueless_strings_name_their_source() {
    let f = Fixture::new("nonbool");
    for key in ["remote.o.pushurl", "remote.pushdefault", "url.foo.insteadof", "branch.main.merge"]
    {
        for cmd in TRANSPORT_VERBS {
            let (out, err, code) = f.with(&[key], cmd);
            let want = format!(
                "error: missing value for '{key}'\n\
                 fatal: unable to parse '{key}' from command-line config\n"
            );
            assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128), "{key} {cmd:?}");
        }
    }
    // The empty branch subsection has no `error()` line of its own.
    let (_, err, code) = f.with(&["branch..remote=x"], &["push", "o", "main"]);
    assert_eq!(
        (err.as_str(), code),
        ("fatal: unable to parse 'branch..remote' from command-line config\n", 128)
    );
}

#[test]
fn nothing_reaches_the_remote_after_a_refusal() {
    let f = Fixture::new("nopush");
    std::fs::write(f.work.join("b"), "b\n").unwrap();
    f.run(&["add", "b"]);
    f.run(&["commit", "-q", "-m", "b"]);
    let before = f.run(&["--git-dir=../up.git", "rev-parse", "main"]).0;
    let (_, _, code) = f.with(&["remote.o.prune=bogus"], &["push", "o", "main"]);
    assert_eq!(code, 128);
    assert_eq!(f.run(&["--git-dir=../up.git", "rev-parse", "main"]).0, before);
}

#[test]
fn a_refusal_only_the_fetch_child_reaches_ends_pull_at_one() {
    let f = Fixture::new("pullchild");
    // `--no-rebase` skips `config_get_rebase()`, so the pull never reads the
    // remote state itself and only its fetch child dies.
    let (out, err, code) = f.with(&["remote.o.prune=bogus"], &["pull", "--no-rebase", "o", "main"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: bad boolean config value 'bogus' for 'remote.o.prune'\n", 1)
    );
    // `--rebase` still reads it in the pull, through `get_rebase_fork_point()`.
    let (_, _, code) = f.with(&["remote.o.prune=bogus"], &["pull", "--rebase", "o", "main"]);
    assert_eq!(code, 128);
}

#[test]
fn non_fatal_diagnostics_print_once_per_process() {
    let f = Fixture::new("warn");
    let dup = ["remote.o.receivepack=git-receive-pack", "remote.o.receivepack=x"];
    let (out, err, code) = f.with(&dup, &["ls-remote", "o"]);
    assert!(out.ends_with("\trefs/heads/main\n"), "{out}");
    assert_eq!(
        (err.as_str(), code),
        ("error: more than one receivepack given, using the first\n", 0)
    );
    let (_, err, code) = f.with(&["remote./x.url=y"], &["fetch", "o"]);
    assert_eq!(
        (err.as_str(), code),
        ("warning: config remote shorthand cannot begin with '/': /x.url\n", 0)
    );
    // `pull` reads the remote state, then its fetch child reads it again.
    let (out, err, code) = f.with(&["remote./x.url=y"], &["pull", "o", "main"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "Already up to date.\n",
            "warning: config remote shorthand cannot begin with '/': /x.url\n\
             warning: config remote shorthand cannot begin with '/': /x.url\n\
             From ../up\n * branch            main       -> FETCH_HEAD\n",
            0
        )
    );
}
