//! Every editor git starts goes through `launch_specified_editor()`
//! (editor.c:60-139), not only `commit`'s.
//!
//! `notes` (builtin/notes.c:222), `merge --edit` (builtin/merge.c:971),
//! `replace --edit` (builtin/replace.c:351), `config --edit`
//! (builtin/config.c:1316), `bugreport` (builtin/bugreport.c:196), the rebase
//! todo list (`launch_sequence_editor()`, rebase-interactive.c:135) and `add -p`'s
//! hunk edit (`strbuf_edit_interactively()`, add-patch.c:1274, editor.c:152-181)
//! all hand the editor `strbuf_realpath()` of the file and leave `p.dir` unset, so
//! the child starts where setup left git with `GIT_PREFIX` exported
//! (setup.c:2069-2076).
//!
//! zvcs kept a private launcher per verb: they handed the editor `../.git/<file>`,
//! ran it in the directory the command was typed in without `GIT_PREFIX`, let
//! `merge` and `add -p` skip an empty `GIT_EDITOR` for `vi`, had `notes` skip
//! reading back the file the `:` editor left, let `config --edit` exit with the
//! editor's status without the `error:` line (git ignores the result and returns
//! 0), and gave `add -p` a fatal of its own wording instead of the two `error:`
//! lines.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Records `<file>|<GIT_DIR>|<GIT_WORK_TREE>|<GIT_PREFIX>|<cwd>` on stderr.
const LOG: &str = "echo \"$1|$GIT_DIR|$GIT_WORK_TREE|$GIT_PREFIX|$(pwd)\" >&2";

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
    /// `main` with `sub/f`, a `side` branch adding `s`, and one more commit on
    /// `main` so merging `side` makes a merge commit.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-editor-shared-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        let f = Fixture { root, work };
        let top = f.work.clone();
        f.git(&top, &[], &["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("sub/f"), "a\n").unwrap();
        f.git(&top, &[], &["add", "."]);
        f.git(&top, &[], &["commit", "-q", "-m", "base"]);
        f.git(&top, &[], &["checkout", "-q", "-b", "side"]);
        std::fs::write(f.work.join("s"), "s\n").unwrap();
        f.git(&top, &[], &["add", "s"]);
        f.git(&top, &[], &["commit", "-q", "-m", "side"]);
        f.git(&top, &[], &["checkout", "-q", "main"]);
        std::fs::write(f.work.join("m"), "m\n").unwrap();
        f.git(&top, &[], &["add", "m"]);
        f.git(&top, &[], &["commit", "-q", "-m", "main2"]);
        f
    }

    fn sub(&self) -> PathBuf {
        self.work.join("sub")
    }

    /// Run in `dir`; returns stdout, stderr (work tree spelled `<W>`) and status.
    fn run(&self, dir: &Path, env: &[(&str, &str)], args: &[&str], stdin: &str) -> (String, String, i32) {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_EDITOR")
            .env_remove("GIT_SEQUENCE_EDITOR")
            .env_remove("VISUAL")
            .env_remove("EDITOR")
            .env("TERM", "xterm")
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
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let w = self.work.to_str().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).replace(w, "<W>"),
            String::from_utf8_lossy(&out.stderr).replace(w, "<W>"),
            out.status.code().expect("no signal"),
        )
    }

    fn git(&self, dir: &Path, env: &[(&str, &str)], args: &[&str]) -> (String, String, i32) {
        self.run(dir, env, args, "")
    }
}

#[test]
fn notes_merge_and_replace_editors_start_at_the_top_with_the_real_path() {
    let f = Fixture::new("verbs");
    let editor = format!("f() {{ {LOG}; echo note >\"$1\"; }}; f");
    let env = [("GIT_EDITOR", editor.as_str())];
    let (_, err, code) = f.git(&f.sub(), &env, &["notes", "add"]);
    assert_eq!((err.as_str(), code), ("<W>/.git/NOTES_EDITMSG|||sub/|<W>\n", 0));
    assert_eq!(f.git(&f.sub(), &[], &["notes", "show"]).0, "note\n");

    let log = format!("f() {{ {LOG}; }}; f");
    let head = f.git(&f.sub(), &[], &["rev-parse", "HEAD"]).0;
    let (_, err, code) = f.git(&f.sub(), &[("GIT_EDITOR", log.as_str())], &["replace", "--edit", "HEAD"]);
    let want = format!(
        "<W>/.git/REPLACE_EDITOBJ|||sub/|<W>\nerror: new object is the same as the old one: '{}'\n",
        head.trim()
    );
    assert_eq!((err, code), (want, 255));

    let (_, err, code) = f.git(&f.sub(), &[("GIT_EDITOR", log.as_str())], &["merge", "-q", "--edit", "side"]);
    assert_eq!((err.as_str(), code), ("<W>/.git/MERGE_MSG|||sub/|<W>\n", 0));
}

#[test]
fn the_sequence_editor_and_config_edit_start_at_the_top() {
    let f = Fixture::new("seq");
    let log = format!("f() {{ {LOG}; }}; f");
    let (_, err, code) = f.git(&f.sub(), &[("GIT_SEQUENCE_EDITOR", log.as_str())], &["rebase", "-i", "HEAD~1"]);
    assert_eq!(
        (err.as_str(), code),
        (
            "<W>/.git/rebase-merge/git-rebase-todo|||sub/|<W>\n\
             Successfully rebased and updated refs/heads/main.\n",
            0
        )
    );
    let (_, err, code) = f.git(&f.sub(), &[("GIT_EDITOR", log.as_str())], &["config", "--edit"]);
    assert_eq!((err.as_str(), code), ("<W>/.git/config|||sub/|<W>\n", 0));
}

#[test]
fn config_edit_reports_a_failed_editor_and_still_returns_zero() {
    let f = Fixture::new("config-fail");
    let (_, err, code) = f.git(&f.sub(), &[("GIT_EDITOR", "false")], &["config", "--edit"]);
    assert_eq!((err.as_str(), code), ("error: there was a problem with the editor 'false'\n", 0));
}

#[test]
fn add_p_hunk_edit_runs_at_the_top_and_reports_a_failed_editor_as_errors() {
    let f = Fixture::new("addp");
    std::fs::write(f.work.join("sub/f"), "a\nb\n").unwrap();
    let log = format!("f() {{ {LOG}; }}; f");
    let (_, err, code) = f.run(&f.sub(), &[("GIT_EDITOR", log.as_str())], &["add", "-p"], "e\n");
    assert_eq!((err.as_str(), code), ("<W>/.git/addp-hunk-edit.diff|||sub/|<W>\n", 0));
    assert_eq!(f.git(&f.sub(), &[], &["diff", "--cached", "--name-only"]).0, "sub/f\n");
    assert!(!f.work.join(".git/addp-hunk-edit.diff").exists());

    f.git(&f.sub(), &[], &["reset", "-q"]);
    let (out, err, code) = f.run(&f.sub(), &[("GIT_EDITOR", "false")], &["add", "-p"], "e\nn\nq\n");
    let mut lines = err.lines();
    assert_eq!(lines.next(), Some("error: there was a problem with the editor 'false'"));
    // `error_errno()` with the errno the editor run left, which is 0 and spelled
    // by the platform's strerror.
    assert!(lines.next().unwrap().starts_with("error: could not edit '.git/addp-hunk-edit.diff': "));
    assert_eq!(lines.next(), None);
    assert!(out.contains("Your edited hunk does not apply. Edit again"), "{out}");
    assert_eq!(code, 0);
}

#[test]
fn an_empty_git_editor_is_the_editor_for_merge() {
    let f = Fixture::new("empty");
    let (_, err, code) = f.git(&f.sub(), &[("GIT_EDITOR", "")], &["merge", "--edit", "side"]);
    assert_eq!(
        (err.as_str(), code),
        (
            "error: cannot run : No such file or directory\n\
             error: unable to start editor ''\n\
             Not committing merge; use 'git commit' to complete the merge.\n",
            1
        )
    );
}

#[test]
fn notes_reads_back_what_the_colon_editor_left() {
    let f = Fixture::new("colon");
    let (_, err, code) = f.git(&f.sub(), &[("GIT_EDITOR", ":")], &["notes", "add", "--no-stripspace"]);
    assert_eq!((err.as_str(), code), ("", 0));
    let (note, _, code) = f.git(&f.sub(), &[], &["notes", "show"]);
    assert_eq!(code, 0);
    assert!(note.starts_with("\n#\n# Write/edit the notes for the following object:\n"), "{note:?}");
}

#[test]
fn bugreport_exports_the_prefix_it_started_in() {
    let f = Fixture::new("bugreport");
    let log = format!("f() {{ {LOG}; }}; f");
    let out = f.root.join("br");
    let (_, err, code) = f.git(
        &f.sub(),
        &[("GIT_EDITOR", log.as_str())],
        &["bugreport", "-s", "x", "-o", out.to_str().unwrap()],
    );
    let root = f.root.to_str().unwrap();
    assert_eq!(
        (err.replace(root, "<R>").as_str(), code),
        (
            "Created new report at '<R>/br/git-bugreport-x.txt'.\n\
             <R>/br/git-bugreport-x.txt|||sub/|<W>\n",
            0
        )
    );
}
