//! `apply`'s binary payloads, whitespace actions and context reduction, and `am` over
//! a multi-patch mbox.
//!
//! A `GIT binary patch` carries a base85-armoured deflate stream — either the whole
//! post-image (`literal`) or a delta against the pre-image — and both ends are checked
//! against the ids the `index` line names. `--whitespace` reports every added line that
//! violates `core.whitespace`, and `fix` rewrites them. `-C<n>` lets a hunk shed context
//! until it lands. `am` splits an mbox at its `From ` postmarks.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-applybin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.cmd(args).output().unwrap()
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn read(&self, path: &str) -> Vec<u8> {
        std::fs::read(self.work.join(path)).unwrap()
    }
}

/// A `GIT binary patch` rebuilds the file from its payload, and both ends are checked
/// against the ids the `index` line names.
///
/// The patch below is stock git 2.55.0's own output for a 40-byte file whose sixth byte
/// changed and which grew three bytes — the `literal` form, forward payload first and
/// the reverse second.
#[test]
fn binary_patches_rebuild_the_file() {
    const PATCH: &str = "diff --git a/bin.dat b/bin.dat\nindex b7412a3233145934f87243bd85ef180288b87be5..faf0008f53c2abb1442be3e02b8927440ae8a520 100644\nGIT binary patch\nliteral 43\nacmZQzWMXDn#m3IT$pB)p;$m@fasmJ-8v%6y\n\nliteral 40\nUcmZQzWMXDvWn<^yWWdV;01Ze0wEzGB\n\n";

    let before: Vec<u8> = (0u8..10).cycle().take(40).collect();
    let mut after = before.clone();
    after[5] = 0xAA;
    after.extend_from_slice(&[9, 9, 9]);

    let f = Fixture::new("binary");
    f.write("bin.dat", &before);
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "seed"]);
    f.write("bin.patch", PATCH.as_bytes());

    let out = f.run(&["apply", "--check", "bin.patch"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let out = f.run(&["apply", "bin.patch"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(f.read("bin.dat"), after, "the payload rebuilt the wrong bytes");

    // Applying it to the wrong pre-image is refused, naming the id it wanted.
    f.write("bin.dat", b"something else\n");
    let out = f.run(&["apply", "--check", "bin.patch"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("does not match the current contents"),
        "{out:?}"
    );
}

/// `--whitespace` reports the offending added lines; `error` refuses, `fix` rewrites.
#[test]
fn whitespace_actions_report_and_fix() {
    let f = Fixture::new("ws");
    f.write("main.c", b"int main(void);\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "seed"]);
    let patch = "diff --git a/main.c b/main.c\n--- a/main.c\n+++ b/main.c\n@@ -1 +1,3 @@\n int main(void);\n+int trailing(void);  \n+ \tint indented(void);\n";
    f.write("ws.patch", patch.as_bytes());

    let out = f.run(&["apply", "--whitespace=error", "ws.patch"]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    let msg = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(msg.contains("ws.patch:6: trailing whitespace."), "{msg}");
    assert!(
        msg.contains("ws.patch:7: space before tab in indent."),
        "{msg}"
    );
    assert!(msg.ends_with("error: 2 lines add whitespace errors.\n"), "{msg}");
    assert_eq!(f.read("main.c"), b"int main(void);\n", "nothing was written");

    // `warn` (the default) reports and applies.
    let out = f.run(&["apply", "--whitespace=warn", "ws.patch"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr)
        .ends_with("warning: 2 lines add whitespace errors.\n"));
    assert_eq!(
        f.read("main.c"),
        b"int main(void);\nint trailing(void);  \n \tint indented(void);\n",
        "warn applies the lines verbatim"
    );

    // `fix` strips the trailing run and the spaces in front of the tab.
    f.git(&["checkout", "-q", "--", "main.c"]);
    let out = f.run(&["apply", "--whitespace=fix", "ws.patch"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr)
        .ends_with("warning: 2 lines applied after fixing whitespace errors.\n"));
    assert_eq!(
        f.read("main.c"),
        b"int main(void);\nint trailing(void);\n\tint indented(void);\n"
    );
}

/// `-C<n>` lets a hunk shed context lines until it lands.
#[test]
fn context_reduction_places_a_shifted_hunk() {
    let f = Fixture::new("ctx");
    f.write("f.txt", b"one\ntwo\nthree\nfour\nfive\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "seed"]);
    // The patch expects `two`/`three` above and `four`/`five` below, but the file has
    // lost `two`, so the full context no longer matches.
    let patch = "diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,5 +1,6 @@\n one\n two\n three\n+inserted\n four\n five\n";
    f.write("patch.diff", patch.as_bytes());
    f.write("f.txt", b"one\nthree\nfour\nfive\n");

    let out = f.run(&["apply", "--check", "patch.diff"]);
    assert_eq!(out.status.code(), Some(1), "full context must not apply: {out:?}");

    let out = f.run(&["apply", "-C1", "patch.diff"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(f.read("f.txt"), b"one\nthree\ninserted\nfour\nfive\n");
}

/// `am` splits an mbox at its postmarks and applies each message in order.
#[test]
fn am_applies_every_patch_in_an_mbox() {
    let f = Fixture::new("mbox");
    f.write("f.txt", b"one\n");
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "seed"]);
    let base = String::from_utf8_lossy(&f.run(&["rev-parse", "HEAD"]).stdout)
        .trim_end()
        .to_owned();
    f.write("f.txt", b"one\ntwo\n");
    f.git(&["commit", "-qam", "add two"]);
    f.write("f.txt", b"one\ntwo\nthree\n");
    f.git(&["commit", "-qam", "add three"]);

    let series = f.run(&["format-patch", "--stdout", &format!("{base}..HEAD")]);
    assert!(series.status.success(), "{series:?}");
    std::fs::write(f.work.join("series.mbox"), &series.stdout).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&series.stdout).matches("\nSubject: [PATCH").count(),
        2,
        "the fixture needs two patches"
    );

    f.git(&["reset", "-q", "--hard", &base]);
    let out = f.run(&["am", "series.mbox"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Applying: add two\nApplying: add three\n"
    );
    assert_eq!(f.read("f.txt"), b"one\ntwo\nthree\n");
    let subjects = String::from_utf8_lossy(&f.run(&["log", "--format=%s", "-2"]).stdout)
        .trim_end()
        .to_owned();
    assert_eq!(subjects, "add three\nadd two");
}
