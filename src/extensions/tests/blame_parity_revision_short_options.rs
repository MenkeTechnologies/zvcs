//! `git blame`'s short revision and diff options, compared with stock git.
//!
//! `cmd_blame()` hands every option its own `options[]` does not claim to
//! `parse_revision_opt()` (builtin/blame.c:1014-1060), so a short word blame
//! does not own reaches `handle_revision_opt()` (revision.c:2768-2780): its
//! `strcmp()` chain takes a lone `-r`, `-t`, `-v`, `-m`, `-E`, `-i`, `-F` or
//! `-P`, and anything else goes to `diff_opt_parse()` (revision.c:2758-2762),
//! a `PARSE_OPT_ONE_SHOT` pass over the word's characters (diff.c:6303-6321).
//! zvcs refused every one of them as an unported option, where git accepts
//! the ones it knows and prints nothing different for them.
//!
//! What git refuses it refuses in its own words: an unknown character is
//! `unknown option` naming the slot as `parse_options_step()` rewrote it
//! (`-ux` names `-x`, parse-options.c:1090-1096) or `(null)` once an earlier
//! word was consumed (revision.c:2330-2346), and the value parsers answer for
//! `-l` (parse-options.c:260-289), `-I` and `--ignore-matching-lines`
//! (diff.c:5838-5856), and `-X`/`--dirstat` (diff.c:5433-5447, :5685-5699).

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

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

fn git(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (Vec<u8>, Vec<u8>, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0100")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0100")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (out.stdout, out.stderr, out.status.code().expect("no signal"))
}

impl Fixture {
    /// Two copies of one history built by stock git: `f.c` committed, then
    /// line 2 rewritten.
    fn new(stock: &str, tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-blame-short-opts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for side in ["stock", "zvcs"] {
            let work = root.join(side);
            std::fs::create_dir_all(&work).unwrap();
            let run = |args: &[&str]| {
                let (_, err, rc) = git(stock, &work, &root, args);
                assert_eq!(rc, 0, "{args:?}: {}", String::from_utf8_lossy(&err));
            };
            run(&["init", "-q", "-b", "main", "."]);
            std::fs::write(work.join("f.c"), "int a;\nint b;\nint c;\n").unwrap();
            run(&["add", "f.c"]);
            run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
            std::fs::write(work.join("f.c"), "int a;\nint B;\nint c;\n").unwrap();
            run(&["-c", "maintenance.auto=false", "commit", "-q", "-am", "two"]);
        }
        Fixture { root }
    }

    fn assert_same(&self, stock: &str, args: &[&str]) {
        let s = git(stock, &self.root.join("stock"), &self.root, args);
        let z = git(BIN, &self.root.join("zvcs"), &self.root, args);
        assert_eq!(
            (String::from_utf8_lossy(&z.0), String::from_utf8_lossy(&z.1), z.2),
            (String::from_utf8_lossy(&s.0), String::from_utf8_lossy(&s.1), s.2),
            "git {args:?}"
        );
    }
}

#[test]
fn short_words_handle_revision_opt_knows_are_accepted() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "known");
    for word in [
        "-a", "-i", "-m", "-r", "-u", "-v", "-z", "-D", "-E", "-F", "-P", "-R", "-W", "-X",
        "-Xfiles", "-Ia", "-Ga", "-Oa", "-pz", "-zp", "-ul5", "-uU5", "-uB", "-uM5", "-Wl3",
    ] {
        f.assert_same(stock, &["blame", word, "-s", "HEAD", "--", "f.c"]);
        f.assert_same(stock, &["blame", "-w", word, "HEAD", "--", "f.c"]);
    }
    // A required value not attached to its switch is the next argv slot.
    f.assert_same(stock, &["blame", "-G", "-s", "HEAD", "--", "f.c"]);
    f.assert_same(stock, &["blame", "-O", "x", "-s", "HEAD", "--", "f.c"]);
    f.assert_same(stock, &["blame", "-ul", "-1k", "-s", "HEAD", "--", "f.c"]);
}

#[test]
fn unknown_short_words_name_the_slot_parse_options_left() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "unknown");
    for args in [
        &["-x"][..],
        &["-xs"],
        &["-sx"],
        &["-ux"],
        &["-Rx"],
        &["-gu"],
        &["-vr"],
        &["-mv"],
        &["-Ex"],
        &["-s", "-x"],
        &["-x", "-s"],
        &["-s", "-ux"],
        &["-L", "1,2", "-x"],
    ] {
        let mut argv = vec!["blame"];
        argv.extend_from_slice(args);
        argv.extend_from_slice(&["HEAD", "--", "f.c"]);
        f.assert_same(stock, &argv);
    }
}

#[test]
fn diff_option_values_are_refused_as_git_refuses_them() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "values");
    for args in [
        &["-ul"][..],
        &["-ulx"],
        &["-ul99999999999"],
        &["-uUx"],
        &["-uMx"],
        &["-uCx"],
        &["-Xa"],
        &["-uXa"],
        &["-uI["],
        &["-I", "["],
        &["--dirstat=a"],
        &["--dirstat-by-file=a"],
        &["--dirstat=files,x"],
        &["--ignore-matching-lines=["],
        &["--ignore-matching-lines", "["],
    ] {
        let mut argv = vec!["blame"];
        argv.extend_from_slice(args);
        argv.extend_from_slice(&["HEAD", "--", "f.c"]);
        f.assert_same(stock, &argv);
    }
    // A required value missing at the end of the line.
    f.assert_same(stock, &["blame", "f.c", "-uG"]);
    f.assert_same(stock, &["blame", "f.c", "-I"]);
}
