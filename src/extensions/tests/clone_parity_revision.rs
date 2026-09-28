//! `git clone --revision=<rev>`: clone what one ref or object id needs and detach `HEAD` there.
//!
//! `cmd_clone()` turns the option into the only fetch refspec, without a destination, and
//! switches tags, `--single-branch` and the remote `HEAD` off (builtin/clone.c:1405-1412). No
//! refspec reaches the configuration (`if (!option_rev) write_refspec_config(...)`, :1590), no
//! ref is stored, and `update_head()` detaches `HEAD` at the commit through
//! `lookup_commit_or_die()` (:586-593), which warns when it had to peel a tag (commit.c:86-89).
//! A revision nothing advertised is `Remote revision %s not found in upstream %s` (:1547-1551);
//! `--branch` and `--mirror` are refused beside it (:1364-1367). zvcs refused the option
//! outright.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `src`: `one` (annotated tag `v1`) and `two` on `main`, `three` on `side`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-clone-revision-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        f.ok(&["init", "-q", "-b", "main", "src"]);
        for (name, msg) in [("a", "one"), ("b", "two")] {
            std::fs::write(f.root.join("src").join(name), format!("{name}\n")).unwrap();
            f.ok(&["-C", "src", "add", name]);
            f.ok(&["-C", "src", "commit", "-q", "-m", msg]);
            if msg == "one" {
                f.ok(&["-C", "src", "tag", "-a", "-m", "t", "v1"]);
            }
        }
        f.ok(&["-C", "src", "checkout", "-q", "-b", "side"]);
        std::fs::write(f.root.join("src/c"), "c\n").unwrap();
        f.ok(&["-C", "src", "add", "c"]);
        f.ok(&["-C", "src", "commit", "-q", "-m", "three"]);
        f.ok(&["-C", "src", "checkout", "-q", "main"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.root)
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

    fn ok(&self, args: &[&str]) -> String {
        let (out, err, code) = self.run(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        out
    }

    fn rev(&self, spec: &str) -> String {
        self.ok(&["-C", "src", "rev-parse", spec]).trim_end().to_string()
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.root.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
    }
}

const ADVICE: &str = "\n\
You are in 'detached HEAD' state. You can look around, make experimental
changes and commit them, and you can discard any commits you make in this
state without impacting any branches by switching back to a branch.

If you want to create a new branch to retain commits you create, you may
do so (now or later) by using -c with the switch command. Example:

  git switch -c <new-branch-name>

Or undo this operation with:

  git switch -

Turn off this advice by setting config variable advice.detachedHead to false

";

#[test]
fn a_branch_is_checked_out_detached_with_no_ref_and_no_refspec() {
    let f = Fixture::new("branch");
    let side = f.rev("side");
    let (out, err, code) = f.run(&["clone", "--revision=side", "src", "d"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "");
    assert_eq!(err, format!("Cloning into 'd'...\ndone.\nNote: switching to '{side}'.\n{ADVICE}"));
    assert_eq!(f.read("d/.git/HEAD"), format!("{side}\n"));
    // The clone records the source as `absolute_pathdup()` resolved it.
    let src = std::fs::canonicalize(f.root.join("src")).unwrap();
    // The remote section is the url alone, right below `[core]`: no refspec and no `tagOpt`.
    let config = f.read("d/.git/config");
    assert!(
        config.ends_with(&format!("true\n[remote \"origin\"]\n\turl = {}\n", src.display())),
        "{config}"
    );
    assert_eq!(f.ok(&["-C", "d", "for-each-ref"]), "");
    assert_eq!(f.read("d/.git/packed-refs"), "# pack-refs with: peeled fully-peeled sorted \n");
    assert_eq!(
        f.read("d/.git/logs/HEAD"),
        format!(
            "0000000000000000000000000000000000000000 {side} C O Mitter <committer@example.com> 1700000000 +0000\tclone: from {}\n",
            src.display()
        )
    );
    assert_eq!(f.ok(&["-C", "d", "ls-files"]), "a\nb\nc\n");
    assert_eq!(f.ok(&["-C", "d", "status", "--porcelain"]), "");
}

#[test]
fn an_annotated_tag_is_peeled_with_a_warning() {
    let f = Fixture::new("tag");
    let tag = f.rev("v1");
    let one = f.rev("v1^{commit}");
    let url = format!("file://{}", f.root.join("src").display());
    let (_, err, code) = f.run(&["-c", "advice.detachedHead=false", "clone", "-q", "--revision=v1", &url, "d"]);
    assert_eq!((err.as_str(), code), (format!("warning: refs/tags/v1 {tag} is not a commit!\n").as_str(), 0));
    assert_eq!(f.read("d/.git/HEAD"), format!("{one}\n"));
    assert_eq!(f.ok(&["-C", "d", "for-each-ref"]), "");
    // Only what the revision needs came across: the tag, its commit, the tree and one blob.
    assert_eq!(f.ok(&["-C", "d", "cat-file", "--batch-all-objects", "--batch-check"]).lines().count(), 4);
}

#[test]
fn a_full_object_id_is_cloned_bare() {
    let f = Fixture::new("oid");
    let side = f.rev("side");
    let (_, err, code) = f.run(&["clone", "--bare", &format!("--revision={side}"), "src", "d.git"]);
    assert_eq!((err.as_str(), code), ("Cloning into bare repository 'd.git'...\ndone.\n", 0));
    assert_eq!(f.read("d.git/HEAD"), format!("{side}\n"));
    assert!(!f.read("d.git/config").contains("fetch"));
}

#[test]
fn what_is_not_advertised_or_combined_is_refused_and_cleaned_up() {
    let f = Fixture::new("refused");
    let short = f.rev("main")[..7].to_string();
    for (args, want) in [
        (vec!["clone", "-q", "--revision=nope", "src", "d"], "fatal: Remote revision nope not found in upstream origin\n".to_string()),
        (
            vec!["clone", "-q", &format!("--revision={short}"), "src", "d"],
            format!("fatal: Remote revision {short} not found in upstream origin\n"),
        ),
        (vec!["clone", "-q", "--revision=side~1", "src", "d"], "fatal: invalid refspec 'side~1'\n".to_string()),
        (
            vec!["clone", "-q", "--revision=side", "--branch=main", "src", "d"],
            "fatal: options '--revision' and '--branch' cannot be used together\n".to_string(),
        ),
        (
            vec!["clone", "-q", "--revision=side", "--mirror", "src", "d"],
            "fatal: options '--revision' and '--mirror' cannot be used together\n".to_string(),
        ),
    ] {
        let (out, err, code) = f.run(&args);
        assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128), "{args:?}");
        assert!(!Path::new(&f.root.join("d")).exists(), "{args:?} left the clone behind");
    }
}
