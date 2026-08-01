//! Moving a ref with no `user.name`/`user.email` configured.
//!
//! Every ref move writes a reflog line, and a reflog line needs a committer.
//! git synthesizes one from the OS (the login name and the hostname) and carries
//! on; it refuses only when nothing at all can be determined. gix errors
//! instead, and its personas are cached when the repository is opened, so the
//! identity has to be seeded before the first ref edit.
//!
//! Without that, a machine with no `~/.gitconfig` — a fresh container, a CI
//! runner, a `sudo` shell, a `HOME` that is not the user's — cannot create a
//! branch or detach `HEAD` at all. The failure that surfaced it was worse than
//! that: `git submodule update --init` runs a checkout per submodule, so the
//! first one to fail ended the walk and left every later submodule
//! unpopulated, which reads as "`--init` did nothing".
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-ident-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let f = Fixture { root: root.clone(), repo: root.join("repo") };
        std::fs::create_dir_all(&f.repo).unwrap();
        f.identity(true);
        f.git(&f.repo.clone(), &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.repo.join("a.txt"), "a\n").unwrap();
        f.git(&f.repo.clone(), &["add", "-A"]);
        f.git(&f.repo.clone(), &["commit", "-q", "-m", "c0"]);
        f
    }

    /// Write (or empty) the global config the commands will read. The fixture's
    /// own commits need an identity; the commands under test must not.
    fn identity(&self, on: bool) {
        let body = if on {
            "[user]\n\tname = t\n\temail = t@e.co\n[protocol \"file\"]\n\tallow = always\n"
        } else {
            "[protocol \"file\"]\n\tallow = always\n"
        };
        std::fs::write(self.root.join("gitconfig"), body).unwrap();
    }

    fn cmd(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            // The env identity outranks config, so it has to go too — otherwise
            // the fallback under test is never reached.
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL");
        c
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let out = self.cmd(dir, args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// `(exit code, stdout, stderr)` of a command run in the repo.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let repo = self.repo.clone();
        let out = self.cmd(&repo, args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// Each of these moves a ref, so each writes a reflog; none of them may need an
/// identity the user has not given.
#[test]
fn ref_moves_work_without_a_configured_identity() {
    let f = Fixture::new("refmoves");
    f.identity(false);

    for args in [
        vec!["branch", "newbr"],
        vec!["checkout", "-b", "created"],
        vec!["switch", "-c", "switched"],
        vec!["checkout", "-B", "forced"],
        vec!["update-ref", "refs/heads/plumbed", "HEAD"],
        vec!["checkout", "--detach", "main"],
    ] {
        let (code, out, err) = f.run(&args);
        assert_eq!(code, 0, "`git {args:?}` failed: {out}{err}");
        assert!(
            !err.contains("committer"),
            "`git {args:?}` complained about the identity: {err}"
        );
    }
}

/// The reflog is written, not skipped — the synthesized identity is a real one.
#[test]
fn the_synthesized_identity_reaches_the_reflog() {
    let f = Fixture::new("reflog");
    f.identity(false);

    let (code, out, err) = f.run(&["checkout", "-b", "logged"]);
    assert_eq!(code, 0, "checkout failed: {out}{err}");

    let log = std::fs::read_to_string(f.repo.join(".git/logs/HEAD")).expect("HEAD reflog written");
    assert!(log.contains("checkout: moving from main to logged"), "reflog: {log}");
    // `<name> <email> <ts> <tz>` — the identity is between the two oids and the
    // message, and an empty one would leave `<>` there.
    assert!(!log.contains(" <> "), "the reflog identity must not be empty: {log}");
}

/// A configured identity is used as given: the fallback fills a gap, it does not
/// override.
#[test]
fn a_configured_identity_is_left_alone() {
    let f = Fixture::new("configured");
    let (code, out, err) = f.run(&["checkout", "-b", "named"]);
    assert_eq!(code, 0, "checkout failed: {out}{err}");

    let log = std::fs::read_to_string(f.repo.join(".git/logs/HEAD")).unwrap();
    assert!(log.contains("t <t@e.co>"), "the configured identity must be used: {log}");
}

/// The reported failure: `submodule update --init` checks out each submodule in
/// turn, so one that cannot check out ends the walk and every later submodule is
/// left unpopulated. Both must land.
#[test]
fn a_submodule_walk_populates_every_submodule() {
    let f = Fixture::new("subwalk");
    let root = f.root.clone();

    // Two upstream submodules and a superproject that references both.
    for name in ["one", "two"] {
        let up = root.join(format!("sub{name}"));
        std::fs::create_dir_all(&up).unwrap();
        f.git(&up, &["init", "-q", "-b", "main", "."]);
        std::fs::write(up.join("s.txt"), format!("{name}\n")).unwrap();
        f.git(&up, &["add", "-A"]);
        f.git(&up, &["commit", "-q", "-m", "s0"]);
    }
    let super_dir = root.join("super");
    std::fs::create_dir_all(&super_dir).unwrap();
    f.git(&super_dir, &["init", "-q", "-b", "main", "."]);
    std::fs::write(super_dir.join("t.txt"), "top\n").unwrap();
    f.git(&super_dir, &["add", "-A"]);
    f.git(&super_dir, &["commit", "-q", "-m", "c0"]);
    for name in ["one", "two"] {
        let url = root.join(format!("sub{name}"));
        f.git(&super_dir, &["submodule", "add", "-q", url.to_str().unwrap(), &format!("lib/{name}")]);
    }
    f.git(&super_dir, &["commit", "-q", "-m", "add subs"]);

    let clone = root.join("clone");
    f.git(&root.clone(), &["clone", "-q", super_dir.to_str().unwrap(), clone.to_str().unwrap()]);

    // The identity disappears before the walk — the state a bare runner is in.
    f.identity(false);
    let out = f.cmd(&clone, &["submodule", "update", "--init"]).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "submodule update failed: {err}");

    for name in ["one", "two"] {
        let file = clone.join(format!("lib/{name}/s.txt"));
        assert!(
            file.exists(),
            "lib/{name} was left unpopulated — the walk stopped early:\n{err}"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), format!("{name}\n"));
    }
}
