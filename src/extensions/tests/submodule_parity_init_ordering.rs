//! `git submodule init` is a loop, and git does everything inside it: it writes
//! `submodule.<name>.active`, resolves the url — which may `warning()` — writes
//! `submodule.<name>.url`, and prints `Submodule '<name>' (<url>) registered for
//! path '<path>'`, all for one submodule before it looks at the next
//! (`init_submodule()`, builtin/submodule--helper.c:596-631).
//!
//! zvcs hoisted both halves out of the loop: every `registered` line was held
//! back and printed after the walk, so a two-submodule `init` of a superproject
//! with no default remote printed both `warning:` lines and then both
//! `registered` lines instead of alternating; and the config was written once at
//! the end, so a `die()` partway through discarded the keys git had already
//! committed for the submodules before it.
//!
//! Both expectations below are the measured behaviour of stock git 2.55.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn cmd(dir: &Path, home: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0000")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000")
        .env("LC_ALL", "C");
    c
}

struct World {
    root: PathBuf,
    sup: PathBuf,
}

impl World {
    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        cmd(dir, &self.root, args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    }

    fn ok(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.run(dir, args);
        assert!(
            out.status.success(),
            "git {args:?} failed ({:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Everything `init` is supposed to have registered, in config order.
    fn registered(&self) -> String {
        let out = self.run(
            &self.sup,
            &["config", "--local", "--get-regexp", "^submodule\\."],
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A superproject with two submodules, both registered and then deinit'd, so
/// `init` has two rounds of work left to do. The superproject deliberately has
/// no `remote.origin.url`: that is what makes `resolve_relative_url()` warn.
fn world(tag: &str) -> World {
    let root = std::env::temp_dir().join(format!("zvcs-sminit-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::write(
        root.join("gitconfig"),
        "[protocol \"file\"]\n\tallow = always\n",
    )
    .unwrap();

    let w = World {
        sup: root.join("sup"),
        root,
    };
    for name in ["src1", "src2"] {
        let dir = w.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        w.ok(&dir, &["init", "-q", "-b", "main", "."]);
        std::fs::write(dir.join("a.txt"), format!("{name}\n")).unwrap();
        w.ok(&dir, &["add", "a.txt"]);
        w.ok(&dir, &["commit", "-qm", "c0"]);
    }
    std::fs::create_dir_all(&w.sup).unwrap();
    w.ok(&w.sup, &["init", "-q", "-b", "main", "."]);
    std::fs::write(w.sup.join("f.txt"), "f\n").unwrap();
    w.ok(&w.sup, &["add", "f.txt"]);
    w.ok(&w.sup, &["commit", "-qm", "base"]);
    // Relative urls: only those go through `resolve_relative_url()`.
    w.ok(&w.sup, &["submodule", "add", "../src1", "s1"]);
    w.ok(&w.sup, &["submodule", "add", "../src2", "s2"]);
    w.ok(&w.sup, &["commit", "-qm", "add two"]);
    w.ok(&w.sup, &["submodule", "deinit", "-f", "--all"]);
    w
}

/// Stock git 2.55.0, both submodules unregistered and no default remote:
///
/// ```text
/// warning: could not look up configuration 'remote.origin.url'. …
/// Submodule 's1' (<root>/src1) registered for path 's1'
/// warning: could not look up configuration 'remote.origin.url'. …
/// Submodule 's2' (<root>/src2) registered for path 's2'
/// ```
#[test]
fn init_interleaves_the_relative_url_warning_with_each_registered_line() {
    let w = world("order");
    let out = w.run(&w.sup, &["submodule", "init"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "",
        "both lines belong on stderr"
    );

    let warning = "warning: could not look up configuration 'remote.origin.url'. \
                   Assuming this repository is its own authoritative upstream.\n";
    assert_eq!(
        stderr_of(&out),
        format!(
            "{warning}Submodule 's1' ({}/src1) registered for path 's1'\n\
             {warning}Submodule 's2' ({}/src2) registered for path 's2'\n",
            w.root.display(),
            w.root.display()
        )
    );
}

/// `repo_config_set_gently()` writes as it goes, so the keys registered before a
/// `die()` survive it. Here `s1`'s url is removed from `.gitmodules`, which makes
/// `init` fail on the first submodule — but only *after* it has set that
/// submodule's `active` flag.
#[test]
fn init_keeps_the_keys_it_wrote_before_dying_on_a_missing_url() {
    let w = world("partial");
    w.ok(
        &w.sup,
        &["config", "-f", ".gitmodules", "--unset", "submodule.s1.url"],
    );

    let out = w.run(&w.sup, &["submodule", "init"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr_of(&out),
        "fatal: No url found for submodule path 's1' in .gitmodules\n"
    );
    // `active` for `s1` was written before the url lookup failed; `s2` was never
    // reached, so nothing of its own is registered.
    assert_eq!(w.registered(), "submodule.s1.active true\n");

    // Removing the whole mapping instead fails one step earlier, before anything
    // is written for that submodule.
    let w = world("nomapping");
    w.ok(
        &w.sup,
        &["config", "-f", ".gitmodules", "--remove-section", "submodule.s1"],
    );
    let out = w.run(&w.sup, &["submodule", "init"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr_of(&out),
        "fatal: No url found for submodule path 's1' in .gitmodules\n"
    );
    assert_eq!(w.registered(), "");
}
