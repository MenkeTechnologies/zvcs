//! The editor git starts for a message is handed the file's real path and runs
//! where setup left git, with the environment setup exported.
//!
//! `launch_specified_editor()` passes `strbuf_realpath(&realpath, path, 1)` as
//! the editor's argument (editor.c:88-90) and leaves `p.dir` unset, so the child
//! inherits git's cwd — the top of the work tree once setup has moved there, or
//! the directory `setup_explicit_git_dir()` / `setup_work_tree()` settled on
//! (setup.c:496-513, :1107-1205) — along with `GIT_PREFIX` (setup.c:2069-2076)
//! and the `GIT_DIR` / `GIT_WORK_TREE` setup exported. `commit`, `branch
//! --edit-description`, `tag -a`, `add -e`, `am -i` and `history` share this
//! launcher.
//!
//! zvcs handed the editor `<cwd>/../.git/COMMIT_EDITMSG` and ran it in the
//! directory the command was typed in, without `GIT_PREFIX` and with
//! `GIT_WORK_TREE` as typed.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.


use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
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
            .join(format!("zvcs-editor-child-setup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // git reports the physical path (`strbuf_getcwd()`), so the expectations
        // are built on the resolved temp directory.
        let root = std::fs::canonicalize(&root).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::create_dir_all(work.join("out")).unwrap();
        let f = Fixture { root, work };
        f.run_in(&f.work, &[], &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("sub/f"), "f\n").unwrap();
        f.run_in(&f.work, &[], &["add", "."]);
        f.run_in(&f.work, &[], &["commit", "-q", "-m", "base"]);
        f
    }

    fn run_in(&self, dir: &Path, env: &[(&str, &str)], args: &[&str]) -> (String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env(
                "GIT_EDITOR",
                "f() { echo \"$1|$GIT_DIR|$GIT_WORK_TREE|$GIT_PREFIX|$(pwd)\" >&2; echo msg >\"$1\"; }; f",
            )
            .envs(env.iter().copied())
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stderr).replace(self.work.to_str().unwrap(), "<W>"),
            out.status.code().expect("no signal"),
        )
    }

    fn commit(&self, dir: &str, env: &[(&str, &str)], global: &[&str]) -> (String, i32) {
        let mut args = global.to_vec();
        args.extend(["commit", "-q", "--allow-empty"]);
        self.run_in(&self.work.join(dir), env, &args)
    }
}

#[test]
fn the_editor_gets_the_realpath_and_starts_at_the_top_with_git_prefix() {
    let f = Fixture::new("subdir");
    assert_eq!(f.commit("sub", &[], &[]), ("<W>/.git/COMMIT_EDITMSG|||sub/|<W>\n".into(), 0));
}

#[test]
fn the_editor_sees_the_git_dir_and_work_tree_setup_exported() {
    let f = Fixture::new("explicit");
    assert_eq!(
        f.commit("sub", &[], &["--work-tree=.."]),
        ("<W>/.git/COMMIT_EDITMSG|<W>/.git|.|sub/|<W>\n".into(), 0)
    );
    assert_eq!(
        f.commit("sub", &[("GIT_DIR", "../.git")], &[]),
        ("<W>/.git/COMMIT_EDITMSG|../.git|||<W>/sub\n".into(), 0)
    );
    assert_eq!(
        f.commit("out", &[], &["--git-dir=../.git", "--work-tree=../sub"]),
        ("<W>/.git/COMMIT_EDITMSG|<W>/out/../.git|.||<W>/sub\n".into(), 0)
    );
}

#[test]
fn branch_and_tag_editors_run_the_same_way() {
    let f = Fixture::new("branch-tag");
    let sub = f.work.join("sub");
    assert_eq!(
        f.run_in(&sub, &[], &["branch", "--edit-description"]),
        ("<W>/.git/EDIT_DESCRIPTION|||sub/|<W>\n".into(), 0)
    );
    assert_eq!(
        f.run_in(&sub, &[], &["tag", "-a", "t"]),
        ("<W>/.git/TAG_EDITMSG|||sub/|<W>\n".into(), 0)
    );
}
