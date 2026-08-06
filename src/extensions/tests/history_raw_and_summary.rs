//! `--raw`, `--summary`, `-z` and the abbreviation knobs across `log`/`show`.
//!
//! `diff_setup_done()` decides which of these formats survive together: `--name-only`
//! and `--name-status` clear every other one, while `--raw` stacks with the count
//! formats and the patch. `--no-abbrev` is `revs->abbrev = 0`, which the raw columns
//! read as "print the whole id" while the patch `index` line falls back to the
//! configured default. `-z` (`line_termination = 0`) turns every raw/name field and
//! record separator into NUL and stops the C-quoting.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

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
    /// Two commits: a seed, then one that renames a file (with a mode change), adds a
    /// second one, and names a path that has to be C-quoted.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rawsum-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("f.txt", b"a\nb\nc\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "seed"]);

        std::fs::rename(f.work.join("f.txt"), f.work.join("g.txt")).unwrap();
        f.write("g.txt", b"a\nb\nc\nd\n");
        std::fs::set_permissions(
            f.work.join("g.txt"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        f.write("tab\there.txt", b"q\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "move"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn stdout(&self, args: &[&str]) -> Vec<u8> {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        out.stdout
    }

    fn text(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.stdout(args)).into_owned()
    }
}

/// The raw record: modes, abbreviated ids, the status letter with a rename's score,
/// and both C-quoted names.
#[test]
fn raw_records_carry_modes_ids_and_quoted_names() {
    let f = Fixture::new("raw");
    let out = f.text(&["log", "-1", "--format=", "--raw"]);
    let rename = out
        .lines()
        .find(|l| l.contains("R100") || l.contains("R0"))
        .unwrap_or_else(|| panic!("no rename record in:\n{out}"));
    // `:<old mode> <new mode> <old sha> <new sha> R<score>\t<old>\t<new>`.
    assert!(rename.starts_with(":100644 100755 "), "{rename}");
    let cols: Vec<&str> = rename.split('\t').collect();
    assert_eq!(cols.len(), 3, "rename names both sides: {rename}");
    assert_eq!(cols[1], "f.txt");
    assert_eq!(cols[2], "g.txt");

    // A name needing C-quoting is quoted, exactly as `write_name_quoted()` does.
    assert!(out.contains("A\t\"tab\\there.txt\""), "{out}");
    // `show` renders the same records.
    assert_eq!(f.text(&["show", "--format=", "--raw", "HEAD"]), out);
}

/// `--name-only`/`--name-status` clear `--raw`; `--raw` leaves the count formats and
/// the patch alone.
#[test]
fn name_formats_displace_raw_but_raw_stacks_with_the_rest() {
    let f = Fixture::new("stack");
    let displaced = f.text(&["log", "-1", "--format=", "--raw", "--name-only"]);
    assert!(!displaced.contains(":100644"), "name-only wins: {displaced}");
    assert!(displaced.contains("g.txt"), "{displaced}");

    let stacked = f.text(&["log", "-1", "--format=", "--raw", "--numstat", "--summary"]);
    assert!(stacked.contains(":100644 100755 "), "raw survives: {stacked}");
    assert!(stacked.contains("1\t0\tf.txt => g.txt"), "numstat too: {stacked}");
    assert!(stacked.contains(" rename f.txt => g.txt (75%)"), "summary too: {stacked}");
    // `show_rename_copy()` prints the mode change on its own line, without a name.
    assert!(stacked.contains(" mode change 100644 => 100755\n"), "{stacked}");
}

/// `--no-abbrev` widens the raw ids to the full hash but leaves the patch `index`
/// line at the configured default; `--abbrev=<n>` moves both.
#[test]
fn no_abbrev_widens_the_raw_columns_only() {
    let f = Fixture::new("abbrev");
    let full = f.text(&["log", "-1", "--format=", "--raw", "-p", "--no-abbrev"]);
    let raw_line = full.lines().find(|l| l.starts_with(':')).unwrap();
    let old_id = raw_line.split(' ').nth(2).unwrap();
    assert_eq!(old_id.len(), 40, "raw id is the whole hash: {raw_line}");
    let index_line = full.lines().find(|l| l.starts_with("index ")).unwrap();
    let short = index_line["index ".len()..].split("..").next().unwrap();
    assert_eq!(short.len(), 7, "index line keeps the default width: {index_line}");

    let narrow = f.text(&["log", "-1", "--format=", "--raw", "-p", "--abbrev=4"]);
    let raw_line = narrow.lines().find(|l| l.starts_with(':')).unwrap();
    assert_eq!(raw_line.split(' ').nth(2).unwrap().len(), 4, "{raw_line}");
    let index_line = narrow.lines().find(|l| l.starts_with("index ")).unwrap();
    assert_eq!(index_line["index ".len()..].split("..").next().unwrap().len(), 4);

    // `strtoul` reads no number out of a word, and the clamp floors it at 4.
    let bogus = f.text(&["log", "-1", "--format=", "--raw", "--abbrev=xyz"]);
    let raw_line = bogus.lines().find(|l| l.starts_with(':')).unwrap();
    assert_eq!(raw_line.split(' ').nth(2).unwrap().len(), 4, "{raw_line}");
}

/// `-z` NUL-terminates the records and stops quoting the paths.
#[test]
fn z_uses_nul_separators_and_raw_paths() {
    let f = Fixture::new("nul");
    let out = f.stdout(&["log", "-1", "--format=%h", "--raw", "-z"]);
    let text = String::from_utf8_lossy(&out);
    // The status letter is followed by NUL rather than the usual tab, and the path
    // keeps its own tab byte instead of being quoted.
    assert!(text.contains("R075\0f.txt\0g.txt\0"), "fields are NUL-separated: {text:?}");
    assert!(text.contains(" A\0tab\there.txt\0"), "path goes out raw: {text:?}");
    assert!(!text.contains('"'), "nothing is C-quoted under -z: {text:?}");
    // The record the pretty format produced is NUL-terminated too.
    let head = out.split(|&b| b == 0).next().unwrap();
    assert!(!head.contains(&b'\n'), "oneline record ends at the NUL: {text:?}");

    // `--numstat -z` prints the counts, then the raw path; a rename prefixes an
    // empty field and names its source first.
    let nums = String::from_utf8_lossy(&f.stdout(&["log", "-1", "--format=", "--numstat", "-z"]))
        .into_owned();
    assert!(nums.contains("1\t0\t\0f.txt\0g.txt\0"), "{nums:?}");
    assert!(nums.contains("1\t0\ttab\there.txt\0"), "{nums:?}");
}
