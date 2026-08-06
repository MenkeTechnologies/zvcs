//! `log`, `show` and `format-patch` report a moved file as a rename.
//!
//! `init_diff_ui_defaults()` turns `diffcore_rename()` on for every porcelain, so a
//! `git mv` commit is one `R` entry — `rename from`/`rename to` in the patch, `R<score>`
//! in `--name-status`, `old => new` (compacted to `dir/{a => b}` where the paths share
//! a prefix) in the stat formats, and a ` rename … (N%)` summary line — instead of a
//! deletion plus an addition. `--no-renames` and `diff.renames=false` turn it back off.
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
    /// One commit that moves `old.txt` to `new.txt` with a one-line edit (89% similar),
    /// moves `pkg/a.txt` to `pkg/b.txt` untouched, and edits `plain.txt` in place.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-renames-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("pkg")).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        let ten: String = (1..=10).map(|n| format!("line {n}\n")).collect();
        f.write("old.txt", ten.as_bytes());
        f.write("plain.txt", b"plain\n");
        f.write("pkg/a.txt", b"x\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "init"]);

        f.git(&["mv", "old.txt", "new.txt"]);
        f.write("new.txt", format!("{ten}line 11\n").as_bytes());
        f.git(&["add", "new.txt"]);
        f.git(&["mv", "pkg/a.txt", "pkg/b.txt"]);
        f.write("plain.txt", b"plain\nedit\n");
        f.git(&["add", "plain.txt"]);
        f.git(&["commit", "-q", "-m", "renames"]);
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

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

#[test]
fn log_patch_names_both_sides_of_a_rename() {
    let f = Fixture::new("log-p");
    let out = f.stdout(&["log", "-p", "-1", "--format=%s"]);
    assert!(out.contains("diff --git a/old.txt b/new.txt\n"), "{out}");
    assert!(out.contains("similarity index 89%\nrename from old.txt\nrename to new.txt\n"), "{out}");
    assert!(out.contains("diff --git a/pkg/a.txt b/pkg/b.txt\n"), "{out}");
    // The unchanged move has no body at all.
    assert!(
        out.contains("similarity index 100%\nrename from pkg/a.txt\nrename to pkg/b.txt\ndiff --git"),
        "{out}"
    );
    // The rename replaced the deletion and the addition it was made of.
    assert!(!out.contains("deleted file mode"), "{out}");
    assert!(!out.contains("new file mode"), "{out}");
}

#[test]
fn the_name_and_stat_formats_report_the_rename() {
    let f = Fixture::new("log-formats");
    // Entries are ordered by the destination path: `new.txt`, `pkg/b.txt`, `plain.txt`.
    assert_eq!(
        f.stdout(&["log", "--name-status", "-1", "--format="]),
        "R089\told.txt\tnew.txt\nR100\tpkg/a.txt\tpkg/b.txt\nM\tplain.txt\n"
    );
    assert_eq!(
        f.stdout(&["log", "--numstat", "-1", "--format="]),
        "1\t0\told.txt => new.txt\n0\t0\tpkg/{a.txt => b.txt}\n1\t0\tplain.txt\n"
    );
    // `--name-only` names the destination alone.
    assert_eq!(
        f.stdout(&["log", "--name-only", "-1", "--format="]),
        "new.txt\npkg/b.txt\nplain.txt\n"
    );
    let stat = f.stdout(&["log", "--stat", "-1", "--format="]);
    assert!(stat.contains(" pkg/{a.txt => b.txt} | 0\n"), "{stat}");
    assert!(stat.contains(" old.txt => new.txt   | 1 +\n"), "{stat}");
    assert!(stat.contains("3 files changed, 2 insertions(+)\n"), "{stat}");
}

#[test]
fn show_reports_the_rename_in_every_format_it_offers() {
    let f = Fixture::new("show");
    let patch = f.stdout(&["show", "--format=%s", "HEAD"]);
    assert!(patch.contains("rename from old.txt\nrename to new.txt\n"), "{patch}");
    let raw = f.stdout(&["show", "--raw", "--format=", "HEAD"]);
    assert!(raw.contains("R089\told.txt\tnew.txt\n"), "{raw}");
    assert!(raw.contains("R100\tpkg/a.txt\tpkg/b.txt\n"), "{raw}");
    let stat = f.stdout(&["show", "--stat", "--format=", "HEAD"]);
    assert!(stat.contains("pkg/{a.txt => b.txt}"), "{stat}");
}

#[test]
fn format_patch_carries_the_rename_headers_and_summary() {
    let f = Fixture::new("format-patch");
    let out = f.stdout(&["format-patch", "-1", "--stdout"]);
    assert!(out.contains(" rename pkg/{a.txt => b.txt} (100%)\n"), "{out}");
    assert!(out.contains(" rename old.txt => new.txt (89%)\n"), "{out}");
    assert!(out.contains("similarity index 89%\nrename from old.txt\nrename to new.txt\n"), "{out}");
    assert!(!out.contains("create mode"), "{out}");
    assert!(!out.contains("delete mode"), "{out}");
}

/// Turning detection off brings the deletion and the addition back.
#[test]
fn no_renames_restores_the_delete_plus_add_shape() {
    let f = Fixture::new("off");
    let out = f.stdout(&["show", "-p", "--no-renames", "--format=", "HEAD"]);
    assert!(out.contains("deleted file mode 100644\n"), "{out}");
    assert!(out.contains("new file mode 100644\n"), "{out}");
    assert!(!out.contains("rename from"), "{out}");

    // `diff.renames=false` is the config spelling of the same thing.
    let out = f.stdout(&["-c", "diff.renames=false", "log", "--name-status", "-1", "--format="]);
    assert_eq!(out, "A\tnew.txt\nD\told.txt\nD\tpkg/a.txt\nA\tpkg/b.txt\nM\tplain.txt\n");
}
