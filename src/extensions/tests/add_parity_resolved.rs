//! `git add --resolved`, new in git 2.56 (builtin/add.c:390-458): stage every
//! conflicted path the pathspec matches — the worktree file at stage 0, or the
//! removal of one that is gone — leave stage-0 entries alone however modified
//! their files are, and refuse the whole run when a selected regular file still
//! carries a conflict marker (`has_conflict_markers()`, merge-ll.c:503-526).
//!
//! Also the 2.56 `die_for_incompatible_opt3()` that replaced the `-A`/`-u` die
//! (builtin/add.c:519-521).
//!
//! The fixture builds the conflict with `update-index --index-info`, so no merge
//! machinery is involved: `c1`, `c2`, `c3` at stages 1-3, `gone` at stages 1 and
//! 3, and `other` at stage 0 with a modified worktree file.
//!
//! Expectations measured against stock git 2.56.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const BASE: &str = "df967b96a579e45a18b8251732d16804b2e56a55";
const MAIN: &str = "ba2906d0666cf726c7eaadd2cd3db615dedfdf3a";
const SIDE: &str = "2299c37978265a95cbe835a4b0f0bbf15aad5549";
const OTHER: &str = "587be6b4c3f93f93c489c0111bba5596147a26cb";

/// The marker-carrying body `c2` and `c3` start with.
const CONFLICTED: &str = "a\n<<<<<<< main\nmain\n=======\nside\n>>>>>>> side\n";

struct Fixture {
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("zvcs-addres-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fx = Fixture { dir };
        assert_eq!(fx.run(&["init", "-q", "-b", "main", "."]).status.code(), Some(0));
        for body in ["base\n", "main\n", "side\n", "x\n"] {
            fx.run_stdin(&["hash-object", "-w", "--stdin"], body);
        }
        let mut info = String::new();
        for path in ["c1", "c2", "c3"] {
            info.push_str(&format!("100644 {BASE} 1\t{path}\n100644 {MAIN} 2\t{path}\n100644 {SIDE} 3\t{path}\n"));
        }
        info.push_str(&format!("100644 {BASE} 1\tgone\n100644 {SIDE} 3\tgone\n100644 {OTHER} 0\tother\n"));
        let out = fx.run_stdin(&["update-index", "--index-info"], &info);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        fx.write("c1", "resolved\n");
        fx.write("c2", CONFLICTED);
        fx.write("c3", CONFLICTED);
        fx.write("gone", "side\n");
        fx.write("other", "other-mod\n");
        fx
    }

    fn write(&self, path: &str, body: &str) {
        std::fs::write(self.dir.join(path), body).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.dir)
            .env("HOME", &self.dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn run_stdin(&self, args: &[&str], input: &str) -> Output {
        use std::io::Write;
        let mut child = self
            .cmd(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    fn staged(&self) -> String {
        String::from_utf8(self.run(&["ls-files", "-s"]).stdout).unwrap()
    }
}

/// The index the fixture starts with, which every refusal must leave in place.
fn untouched() -> String {
    let mut s = String::new();
    for path in ["c1", "c2", "c3"] {
        s.push_str(&format!("100644 {BASE} 1\t{path}\n100644 {MAIN} 2\t{path}\n100644 {SIDE} 3\t{path}\n"));
    }
    s.push_str(&format!("100644 {BASE} 1\tgone\n100644 {SIDE} 3\tgone\n100644 {OTHER} 0\tother\n"));
    s
}

fn text(b: &[u8]) -> &str {
    std::str::from_utf8(b).unwrap()
}

#[test]
fn leftover_markers_refuse_every_path() {
    let fx = Fixture::new("markers");
    let out = fx.run(&["add", "--resolved"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(
        text(&out.stderr),
        "fatal: the following paths still have conflict markers:\n\tc2\n\tc3\n\n"
    );
    // `c1` was resolved, but the check runs over every selected path first.
    assert_eq!(fx.staged(), untouched());
}

#[test]
fn resolved_paths_are_staged_and_the_rest_left_alone() {
    let fx = Fixture::new("stage");
    // `<<<<<<<` and `>>>>>>>` need a space after them (merge-ll.c:493-495); a
    // final `=======` without its newline is one byte short.
    fx.write("c2", "a\n<<<<<<<\tx\n>>>>>>>\n=======");
    std::fs::remove_file(fx.dir.join("c3")).unwrap();
    let out = fx.run(&["add", "--resolved", "-v"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "add 'c1'\nadd 'c2'\nremove 'c3'\nadd 'gone'\n");
    assert_eq!(text(&out.stderr), "");
    assert_eq!(
        fx.staged(),
        format!(
            "100644 2ab19ae607aabda796309682e0448237aab03047 0\tc1\n\
             100644 a479b4f3ae61522ca717c7f34433977fd22aa0a6 0\tc2\n\
             100644 {SIDE} 0\tgone\n\
             100644 {OTHER} 0\tother\n"
        )
    );
}

#[test]
fn dry_run_reports_without_staging() {
    let fx = Fixture::new("dry");
    fx.write("c2", "a\n<<<<<<<\tx\n>>>>>>>\n=======");
    std::fs::remove_file(fx.dir.join("c3")).unwrap();
    let out = fx.run(&["add", "--resolved", "-n"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "add 'c1'\nadd 'c2'\nremove 'c3'\nadd 'gone'\n");
    assert_eq!(fx.staged(), untouched());
}

#[test]
fn the_pathspec_limits_the_conflicted_paths() {
    let fx = Fixture::new("spec");
    let out = fx.run(&["add", "--resolved", "c1", "other"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let expected = untouched().replace(
        &format!("100644 {BASE} 1\tc1\n100644 {MAIN} 2\tc1\n100644 {SIDE} 3\tc1\n"),
        "100644 2ab19ae607aabda796309682e0448237aab03047 0\tc1\n",
    );
    assert_eq!(fx.staged(), expected);

    // `cmd_add()`'s own pathspec loop still dies on an element that names nothing.
    let out = fx.run(&["add", "--resolved", "nosuch"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(text(&out.stderr), "fatal: pathspec 'nosuch' did not match any files\n");
}

#[test]
fn conflict_marker_size_attribute_sets_the_run_length() {
    let fx = Fixture::new("size");
    fx.write(".gitattributes", "c2 conflict-marker-size=9\n");
    let out = fx.run(&["add", "--resolved", "c1", "c2"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stderr), "");
    assert!(
        fx.staged().contains("100644 544ad9ec9bfb5a69bf230a0822dfcd83790f59ec 0\tc2\n"),
        "{}",
        fx.staged()
    );

    // `ll_merge_marker_size()` warns about a value `strtol_i()` refuses and
    // falls back to 7.
    let fx2 = Fixture::new("size-bogus");
    fx2.write(".gitattributes", "c2 conflict-marker-size=bogus\n");
    let out = fx2.run(&["add", "--resolved", "c2"]);
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        text(&out.stderr),
        "warning: invalid marker-size 'bogus', expecting an integer\n\
         fatal: the following paths still have conflict markers:\n\tc2\n\n"
    );
}

#[test]
fn a_binary_line_ends_the_marker_scan() {
    let fx = Fixture::new("binary");
    fx.write("c2", "bin\0\n=======\n");
    let out = fx.run(&["add", "--resolved", "c2"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        fx.staged().contains("100644 21225cae437e48ba5b7480b0f850142a377252b8 0\tc2\n"),
        "{}",
        fx.staged()
    );
}

#[test]
fn intent_to_add_records_the_empty_blob() {
    let fx = Fixture::new("ita");
    std::fs::remove_file(fx.dir.join("c3")).unwrap();
    let out = fx.run(&["add", "-N", "-v", "--resolved", "c1", "c3"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "add 'c1'\nremove 'c3'\n");
    let staged = fx.staged();
    assert!(staged.starts_with("100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0\tc1\n"), "{staged}");
    assert!(!staged.contains("\tc3\n"), "{staged}");
}

#[test]
fn incompatible_options_name_both_spellings() {
    let fx = Fixture::new("opts");
    for (args, msg) in [
        (&["add", "-A", "-u"][..], "fatal: options '-u/--update' and '-A/--all' cannot be used together\n"),
        (&["add", "--resolved", "-u"][..], "fatal: options '-u/--update' and '--resolved' cannot be used together\n"),
        (
            &["add", "-A", "--resolved", "-u"][..],
            "fatal: options '-u/--update', '-A/--all', and '--resolved' cannot be used together\n",
        ),
        // `0 < addremove_explicit`: a later `--no-all` takes `-A` back out.
        (
            &["add", "-A", "--no-all", "-u", "--resolved"][..],
            "fatal: options '-u/--update' and '--resolved' cannot be used together\n",
        ),
        (&["stage", "-u", "-A"][..], "fatal: options '-u/--update' and '-A/--all' cannot be used together\n"),
    ] {
        let out = fx.run(args);
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        assert_eq!(text(&out.stderr), msg, "{args:?}");
    }
    assert_eq!(fx.staged(), untouched());
}
