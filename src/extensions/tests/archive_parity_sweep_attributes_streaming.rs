//! `git archive` corners found by a differential sweep against stock git 2.56.0.
//!
//!   * `$Format:%P$` / `$Format:%p$` in an `export-subst` file walk
//!     `commit->parents` (pretty.c:1586-1599); zvcs rendered them empty.
//!   * `describe_status.max_invocations = 1` (archive.c:766): only the first
//!     `%(describe)` in an archive expands; zvcs expanded every one.
//!   * `:(attr:…)` pathspec magic asks `git_check_attr()` about each entry, and an
//!     exclude carrying it never writes a directory off (tree-walk.c:1183-1200);
//!     zvcs matched such a spec as if no attribute were set.
//!   * `--worktree-attributes` neither unpacks the tree into the index nor sets
//!     `GIT_ATTR_INDEX` (archive.c:526-548), so only a working-copy
//!     `.gitattributes` counts, and a bare repository reads none (attr.c:851-866).
//!   * A plain blob above `core.bigFileThreshold` is streamed raw: no
//!     `convert_to_working_tree()`, and in a zip a `ZIP_STREAM` entry with a data
//!     descriptor (archive.c:216-220, archive-zip.c:343-505). A malformed
//!     threshold dies only when the first such blob is reached.
//!   * A `tar.<name>.command` runs through `prepare_shell_cmd()`, so a bare word
//!     that cannot be exec'd is `unable to start '<cmd>' filter`
//!     (archive-tar.c:497-505); a configured filter claims its `-o` extension
//!     (archive.c:803-827), and `tar.zip.command` shadows the zip writer while
//!     `tar.tar.command` never wins (archive.c:383-395).

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

fn git(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (Vec<u8>, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0200")
        .env("GIT_COMMITTER_DATE", "1700000100 -0500")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (out.stdout, String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code())
}

impl Fixture {
    /// A merge whose `subst.txt` names `%P`/`%p`, a CRLF-converted text file, a
    /// file above a 1-byte threshold, and `.gitattributes` marking `src/` for
    /// `export-subst`. Built with stock git, plus a bare clone.
    fn new(stock: &str, tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-archive-sweep-attrs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("src/deep")).unwrap();
        let f = Fixture { root };
        let g = |args: &[&str]| {
            let mut all = vec!["-c", "maintenance.auto=false"];
            all.extend_from_slice(args);
            let (_, err, code) = git(stock, &work, &f.root, &all);
            assert_eq!(code, Some(0), "{args:?}: {err}");
        };
        g(&["init", "-q", "-b", "main"]);
        std::fs::write(work.join("subst.txt"), "P=$Format:%P$ p=$Format:%p$ d=$Format:%(describe)$\n").unwrap();
        std::fs::write(work.join("src/lib.rs"), "a\nb\n").unwrap();
        std::fs::write(work.join("src/deep/f.txt"), "deep $Format:%h$ $Format:%(describe:tags)$\n").unwrap();
        std::fs::write(work.join(".gitattributes"), "subst.txt export-subst\nsrc/deep/** export-subst\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "base"]);
        g(&["tag", "-a", "v1", "-m", "v1"]);
        g(&["checkout", "-q", "-b", "side"]);
        std::fs::write(work.join("side.txt"), "side\n").unwrap();
        g(&["add", "side.txt"]);
        g(&["commit", "-q", "-m", "side"]);
        g(&["checkout", "-q", "main"]);
        std::fs::write(work.join("main.txt"), "main\n").unwrap();
        g(&["add", "main.txt"]);
        g(&["commit", "-q", "-m", "main"]);
        g(&["merge", "-q", "--no-ff", "-m", "merge", "side"]);
        let (_, err, code) = git(stock, &f.root, &f.root, &["clone", "-q", "--bare", "work", "bare.git"]);
        assert_eq!(code, Some(0), "{err}");
        f
    }

    fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    /// Run `args` in `dir` under both binaries and require identical output.
    fn same_in(&self, stock: &str, dir: &Path, args: &[&str]) {
        let want = git(stock, dir, &self.root, args);
        let got = git(BIN, dir, &self.root, args);
        assert_eq!(got.1, want.1, "stderr of {args:?}");
        assert_eq!(got.2, want.2, "exit code of {args:?}");
        assert!(got.0 == want.0, "stdout of {args:?} differs ({} vs {} bytes)", got.0.len(), want.0.len());
    }

    fn same(&self, stock: &str, args: &[&str]) {
        self.same_in(stock, &self.work(), args)
    }
}

#[test]
fn export_subst_parents_and_attribute_pathspecs() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "subst");
    // Only the first `%(describe)` of the archive runs `git describe`.
    f.same(stock, &["archive", "--format=tar", "HEAD"]);
    f.same(stock, &["archive", "--format=tar", "HEAD", "subst.txt"]);
    f.same(stock, &["archive", "--format=zip", "HEAD", ":(attr:export-subst)"]);
    f.same(stock, &["archive", "--format=tar", "HEAD", ":(attr:!export-subst)"]);
    f.same(stock, &["archive", "--format=tar", "HEAD", "src", ":(exclude,attr:export-subst)"]);
    f.same(stock, &["archive", "--format=tar", "HEAD", ":(exclude,attr:export-subst)"]);
}

#[test]
fn worktree_attributes_read_neither_the_index_nor_a_bare_tree() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "worktree");
    let bare = f.root.join("bare.git");
    f.same_in(stock, &bare, &["archive", "--format=tar", "--worktree-attributes", "HEAD"]);
    f.same_in(stock, &bare, &["archive", "--format=tar", "HEAD"]);
    f.same(stock, &["archive", "--remote=.", "--format=tar", "--worktree-attributes", "HEAD"]);
    std::fs::remove_file(f.work().join(".gitattributes")).unwrap();
    f.same(stock, &["archive", "--format=tar", "--worktree-attributes", "HEAD"]);
}

#[test]
fn big_blobs_are_streamed_unconverted() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "stream");
    for format in ["--format=zip", "--format=tar"] {
        f.same(stock, &["-c", "core.bigFileThreshold=1", "-c", "core.autocrlf=true", "archive", format, "HEAD"]);
    }
    f.same(stock, &["-c", "core.bigFileThreshold=1", "archive", "--format=zip", "-0", "HEAD"]);
    // The threshold is parsed on first use: the directory record `src/` is
    // already out when `src/deep/f.txt` (export-subst, never asked) and then
    // `src/lib.rs` reach the check.
    f.same(stock, &["-c", "core.bigFileThreshold=bogus", "archive", "--format=zip", "-v", "HEAD", "src"]);
    f.same(stock, &["-c", "core.bigFileThreshold=bogus", "archive", "--format=tar", "HEAD", "subst.txt"]);
}

#[test]
fn tar_filters_exec_shadow_and_claim_extensions() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "filter");
    f.same(stock, &["-c", "tar.foo.command=nosuchcmd", "archive", "--format=foo", "HEAD"]);
    f.same(stock, &["-c", "tar.foo.command=nosuch -x", "archive", "--format=foo", "HEAD"]);
    f.same(stock, &["-c", "tar.zip.command=gzip -cn", "archive", "--format=zip", "-11", "HEAD"]);
    f.same(stock, &["-c", "tar.tar.command=gzip -cn", "archive", "--format=tar", "HEAD"]);
    for (name, out) in [("foo", "out.foo"), ("zip", "out.zip"), ("tar", "out.tar")] {
        let key = format!("tar.{name}.command=gzip -cn");
        let args = ["-c", key.as_str(), "archive", "-o", out, "HEAD"];
        assert_eq!(git(stock, &f.work(), &f.root, &args).2, Some(0));
        let want = std::fs::read(f.work().join(out)).unwrap();
        assert_eq!(git(BIN, &f.work(), &f.root, &args).2, Some(0));
        let got = std::fs::read(f.work().join(out)).unwrap();
        assert!(got == want, "-o {out} under {key}");
    }
}
