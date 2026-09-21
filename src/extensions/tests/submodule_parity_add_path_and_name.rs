//! `git submodule add` decides three things before it clones anything — where
//! the submodule goes, what it is called, and whether it may be cloned at all —
//! and every one of them was wrong in zvcs. All expectations below are the
//! measured output of stock git 2.55.0.
//!
//! 1. **The command's directory matters.** `git submodule` runs `cd_to_toplevel`
//!    and re-enters the helper as `git -C "$wt_prefix" submodule--helper add`
//!    (git-submodule.sh:25,144), so `module_add` receives the user's directory as
//!    `prefix` and prepends it to `<path>` (submodule--helper.c:3701-3706). zvcs
//!    ignored `prefix` and put the submodule at the top level instead.
//!
//! 2. **A relative `<repository>` is refused from a subdirectory**
//!    (submodule--helper.c:3710-3712), because `resolve_relative_url()` anchors
//!    on the superproject, not on where the user is standing. zvcs resolved it
//!    against the process's own directory and cloned something else.
//!
//! 3. **`<path>` is normalized**: `normalize_path_copy()` then
//!    `strip_dir_trailing_slashes()` (submodule--helper.c:3728-3729), so
//!    `d/../s2` and `./s2//` are both the submodule `s2`. zvcs passed the raw
//!    spelling to `update-index`, which rejected it.
//!
//! 4. **The name is checked.** A name already used for another path is refused,
//!    and `--force` picks `<name><n>` (submodule--helper.c:3754-3775);
//!    `check_submodule_name()` then rejects a `..` component
//!    (submodule--helper.c:3777, submodule-config.c:214). zvcs did neither, so
//!    `--name ../evil` placed the repository at `.git/evil`.
//!
//! 5. **An existing repository is adopted, not relocated.** `add_submodule()`
//!    prints `Adding existing repo at …` and returns; only the clone branch ends
//!    in `connect_work_tree_and_git_dir()` (submodule--helper.c:3411-3423). zvcs
//!    ran the connect unconditionally and died on the submodule's own `.git`
//!    directory.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn cmd(dir: &Path, home: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args)
        .current_dir(dir)
        .env("HOME", home)
        // `submodule add` clones through a child process, so the permission for
        // a local-path submodule has to live in a config file the child reads.
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

    fn gitmodules(&self) -> String {
        std::fs::read_to_string(self.sup.join(".gitmodules")).unwrap_or_default()
    }
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A superproject with no submodule yet, plus two source repositories beside it
/// (`src1` with two commits, `src2` with one) reachable as plain local paths.
fn world(tag: &str) -> World {
    let root = std::env::temp_dir().join(format!("zvcs-smadd-{tag}-{}", std::process::id()));
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
    for (name, commits) in [("src1", 2usize), ("src2", 1)] {
        let dir = w.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        w.ok(&dir, &["init", "-q", "-b", "main", "."]);
        for i in 0..commits {
            std::fs::write(dir.join("a.txt"), format!("{name}-{i}\n")).unwrap();
            w.ok(&dir, &["add", "a.txt"]);
            w.ok(&dir, &["commit", "-qm", &format!("c{i}")]);
        }
    }
    std::fs::create_dir_all(&w.sup).unwrap();
    w.ok(&w.sup, &["init", "-q", "-b", "main", "."]);
    std::fs::write(w.sup.join("f.txt"), "f\n").unwrap();
    w.ok(&w.sup, &["add", "f.txt"]);
    w.ok(&w.sup, &["commit", "-qm", "base"]);
    w
}

/// From `sup/d`, stock git 2.55.0:
///
/// ```text
/// $ git submodule add ../src2 s2
/// fatal: Relative path can only be used from the toplevel of the working tree
/// $ git submodule add /abs/src2 s2     # .gitmodules gains path = d/s2
/// ```
#[test]
fn add_from_a_subdirectory_prefixes_the_path_and_refuses_a_relative_url() {
    let w = world("prefix");
    let deep = w.sup.join("d");
    std::fs::create_dir_all(&deep).unwrap();

    let out = w.run(&deep, &["submodule", "add", "../../src2", "s2"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr_of(&out),
        "fatal: Relative path can only be used from the toplevel of the working tree\n"
    );
    assert!(!deep.join("s2").exists(), "nothing may be cloned");
    assert_eq!(w.gitmodules(), "", ".gitmodules must not be created");

    // An absolute url is allowed from anywhere, and the recorded path is
    // `prefix` + `<path>` — not the bare `<path>`.
    let src2 = w.root.join("src2");
    w.ok(&deep, &["submodule", "add", src2.to_str().unwrap(), "s2"]);
    assert_eq!(
        w.gitmodules(),
        format!(
            "[submodule \"d/s2\"]\n\tpath = d/s2\n\turl = {}\n",
            src2.display()
        )
    );
    assert!(deep.join("s2/.git").exists(), "cloned under the subdirectory");
    assert!(!w.sup.join("s2").exists(), "and not at the top level");

    // `git add -- .gitmodules` and the gitlink both have to be staged, which
    // only works if those children run at the top of the work tree.
    let staged = w.ok(&w.sup, &["ls-files", "-s"]);
    assert!(staged.contains(".gitmodules"), "staged files: {staged}");
    assert!(staged.contains("160000") && staged.contains("d/s2"), "{staged}");
}

/// `normalize_path_copy()` + `strip_dir_trailing_slashes()`: two spellings of
/// the same path, one `.gitmodules` entry each time, both reading `s2`.
#[test]
fn add_normalizes_the_path_before_recording_it() {
    for (spelling, tag) in [("d/../s2", "dotdot"), ("./s2//", "slashes")] {
        let w = world(tag);
        let src2 = w.root.join("src2");
        w.ok(
            &w.sup,
            &["submodule", "add", src2.to_str().unwrap(), spelling],
        );
        assert_eq!(
            w.gitmodules(),
            format!(
                "[submodule \"s2\"]\n\tpath = s2\n\turl = {}\n",
                src2.display()
            ),
            "spelling {spelling:?}"
        );
        assert!(w.sup.join("s2/.git").exists(), "spelling {spelling:?}");
        assert!(!w.sup.join("d").exists(), "spelling {spelling:?}");
    }
}

/// `submodule_from_name()` finds `s1` bound to a different path, so the second
/// add is refused; `--force` walks `s11`, `s12`, … until one is free.
#[test]
fn add_refuses_a_name_already_bound_to_another_path_and_force_uniquifies_it() {
    let w = world("namereuse");
    let src1 = w.root.join("src1");
    let src2 = w.root.join("src2");
    w.ok(&w.sup, &["submodule", "add", src1.to_str().unwrap(), "s1"]);

    let out = w.run(
        &w.sup,
        &["submodule", "add", "--name", "s1", src2.to_str().unwrap(), "s2"],
    );
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr_of(&out),
        "fatal: submodule name 's1' already used for path 's1'\n"
    );
    assert!(!w.sup.join("s2").exists(), "the refusal is before the clone");

    w.ok(
        &w.sup,
        &["submodule", "add", "-f", "--name", "s1", src2.to_str().unwrap(), "s2"],
    );
    let modules = w.gitmodules();
    assert!(modules.contains("[submodule \"s11\"]\n\tpath = s2\n"), "{modules}");
    assert!(
        w.sup.join(".git/modules/s11").is_dir(),
        "the second submodule needs a git directory of its own"
    );
}

/// `check_submodule_name()` rejects a `..` component, which is the only thing
/// standing between `--name` and a git directory outside `modules/`.
#[test]
fn add_rejects_a_name_with_a_dotdot_component() {
    let w = world("evilname");
    let src2 = w.root.join("src2");
    for name in ["../evil", "a/../../evil", "..", "x/.."] {
        let out = w.run(
            &w.sup,
            &["submodule", "add", "--name", name, src2.to_str().unwrap(), "s2"],
        );
        assert_eq!(out.status.code(), Some(128), "name {name:?}");
        assert_eq!(
            stderr_of(&out),
            format!("fatal: '{name}' is not a valid submodule name\n"),
            "name {name:?}"
        );
        assert!(!w.sup.join("s2").exists(), "name {name:?}: nothing cloned");
        assert!(
            !w.root.join("sup/.git/evil").exists() && !w.root.join("evil").exists(),
            "name {name:?}: nothing written outside modules/"
        );
    }
}

/// `add_submodule()`'s first branch: an existing non-bare repository is adopted
/// where it stands. Its `.git` stays a directory — `absorbgitdirs` is what moves
/// it — so the connect step the clone branch ends with must not run here.
#[test]
fn add_adopts_an_existing_repository_without_moving_its_git_directory() {
    let w = world("adopt");
    let src2 = w.root.join("src2");
    let dest = w.sup.join("s2");
    w.ok(&w.sup, &["clone", "-q", src2.to_str().unwrap(), "s2"]);
    assert!(dest.join(".git").is_dir(), "precondition");

    let out = w.run(&w.sup, &["submodule", "add", src2.to_str().unwrap(), "s2"]);
    assert!(out.status.success(), "add failed: {}", stderr_of(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Adding existing repo at 's2' to the index\n"
    );
    assert!(
        dest.join(".git").is_dir(),
        "the adopted repository keeps its own .git directory"
    );
    assert!(
        !w.sup.join(".git/modules/s2").exists(),
        "and no modules/ entry is created for it"
    );
    let staged = w.ok(&w.sup, &["ls-files", "-s"]);
    assert!(staged.contains("160000") && staged.contains("s2"), "{staged}");

    // A directory that is a repository but not the one being added is still
    // adopted; one holding untracked junk and no repository is in the way.
    let junk = w.sup.join("s3");
    std::fs::create_dir_all(&junk).unwrap();
    std::fs::write(junk.join("j"), "j\n").unwrap();
    let out = w.run(&w.sup, &["submodule", "add", src2.to_str().unwrap(), "s3"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        stderr_of(&out),
        "fatal: 's3' already exists and is not a valid git repo\n"
    );
}
