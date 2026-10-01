//! `git blame` reserves the object-name mark column only when a mark is shown.
//!
//! Up to 2.55 `cmd_blame()` added one to every abbreviation below the hash
//! length "for the boundary commit", so the name column was one wider than the
//! abbreviation even when no line carried a `^`. 2.56 drops that and has
//! `find_alignment()` (builtin/blame.c:708-714) widen the abbreviation by the
//! most marks any entry prints, as counted by `count_marks()`
//! (builtin/blame.c:461-484): `^` for a boundary unless `-b` blanks it or
//! annotate-compat suppresses it, `*` for an unblamable line under
//! `blame.markUnblamableLines`, `?` for an ignored line under
//! `blame.markIgnoredLines`. The widened width is capped at the hash length.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const MARKS: [&str; 4] = [
    "-c",
    "blame.markIgnoredLines=true",
    "-c",
    "blame.markUnblamableLines=true",
];

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
    /// `file` = `one` (root commit), then `two` appended, then `two` rewritten
    /// to `TWO` with `three` appended.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-blame-mark-column-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "one\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "one"]);
        std::fs::write(f.work.join("file"), "one\ntwo\n").unwrap();
        f.run(&["commit", "-q", "-am", "two"]);
        std::fs::write(f.work.join("file"), "one\nTWO\nthree\n").unwrap();
        f.run(&["commit", "-q", "-am", "three"]);
        assert_eq!(
            f.run(&["rev-parse", "HEAD"]).0,
            "e21f2eb4f501f29d30d24c802b08d8ca42238bf9\n",
            "fixture commit ids must match the ones the expectations were measured on"
        );
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
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

    fn blame(&self, config: &[&str], args: &[&str]) -> String {
        let mut argv: Vec<&str> = config.to_vec();
        argv.push("blame");
        argv.push("-s");
        argv.extend_from_slice(args);
        let (out, err, code) = self.run(&argv);
        assert_eq!((err.as_str(), code), ("", 0), "blame {argv:?}");
        out
    }
}

#[test]
fn no_marks_means_no_reserved_column() {
    let f = Fixture::new("none");
    // `--root` leaves no boundary, so nothing is marked and the column is the
    // bare abbreviation.
    assert_eq!(
        f.blame(&[], &["--root", "file"]),
        "2d80382 1) one\ne21f2eb 2) TWO\ne21f2eb 3) three\n"
    );
    // `-b` blanks the boundary instead of marking it, so `count_marks()` does
    // not count it and the blanked run is the bare abbreviation wide.
    assert_eq!(
        f.blame(&[], &["-b", "file"]),
        "        1) one\ne21f2eb 2) TWO\ne21f2eb 3) three\n"
    );
    // annotate-compat never prints `^`.
    assert_eq!(
        f.blame(&[], &["-c", "file"]),
        "2d80382\t(  A U Thor\t2023-11-14 22:13:20 +0000\t1)one\n\
         e21f2eb\t(  A U Thor\t2023-11-14 22:13:20 +0000\t2)TWO\n\
         e21f2eb\t(  A U Thor\t2023-11-14 22:13:20 +0000\t3)three\n"
    );
}

#[test]
fn one_mark_widens_by_one() {
    let f = Fixture::new("one");
    assert_eq!(
        f.blame(&[], &["file"]),
        "^2d80382 1) one\ne21f2eb4 2) TWO\ne21f2eb4 3) three\n"
    );
    assert_eq!(
        f.blame(&[], &["--abbrev=12", "file"]),
        "^2d803824fdb2 1) one\ne21f2eb4f501f 2) TWO\ne21f2eb4f501f 3) three\n"
    );
    // Marks without a boundary: the `?` and `*` lines spend the one reserved
    // column, the unmarked line shows it as a digit.
    assert_eq!(
        f.blame(&MARKS, &["--root", "--ignore-rev", "HEAD", "file"]),
        "2d803824 1) one\n?716ecc2 2) TWO\n*e21f2eb 3) three\n"
    );
}

#[test]
fn two_marks_on_one_line_widen_by_two() {
    let f = Fixture::new("two");
    // The ignored line is passed to the boundary `HEAD~1`, so it carries `^?`.
    assert_eq!(
        f.blame(&MARKS, &["--ignore-rev", "HEAD", "HEAD~1..", "--", "file"]),
        "^716ecc24 1) one\n^?716ecc2 2) TWO\n*e21f2eb4 3) three\n"
    );
    // Under `-b` the boundary is blanked and uncounted; `?` still prints.
    assert_eq!(
        f.blame(&MARKS, &["-b", "--ignore-rev", "HEAD", "HEAD~1..", "--", "file"]),
        "         1) one\n?        2) TWO\n*e21f2eb 3) three\n"
    );
}

#[test]
fn widening_is_capped_at_the_hash_length() {
    let f = Fixture::new("cap");
    // 39 + 2 marks caps at 40; the marks then eat into the digits, exactly as
    // `-l` (which never widens) does.
    let capped = "^716ecc24027c5f1c9c65525dddb4a919118983e 1) one\n\
                  ^?716ecc24027c5f1c9c65525dddb4a919118983 2) TWO\n\
                  *e21f2eb4f501f29d30d24c802b08d8ca42238bf 3) three\n";
    assert_eq!(
        f.blame(&MARKS, &["--abbrev=39", "--ignore-rev", "HEAD", "HEAD~1..", "--", "file"]),
        capped
    );
    assert_eq!(
        f.blame(&MARKS, &["-l", "--ignore-rev", "HEAD", "HEAD~1..", "--", "file"]),
        capped
    );
    assert_eq!(
        f.blame(&[], &["--abbrev=39", "file"]),
        "^2d803824fdb27cc18249512a7e7c64be1eedb78 1) one\n\
         e21f2eb4f501f29d30d24c802b08d8ca42238bf9 2) TWO\n\
         e21f2eb4f501f29d30d24c802b08d8ca42238bf9 3) three\n"
    );
}
