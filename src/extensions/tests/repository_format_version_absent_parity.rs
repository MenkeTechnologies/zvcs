//! `read_repository_format()` (setup.c:866-876, v2.56.0) when the repository's
//! `config` names no `core.repositoryformatversion`: the version stays -1 and
//! `clear_repository_format()` throws away everything `check_repo_format()`
//! collected, so `extensions.objectFormat`, `extensions.refStorage`,
//! `extensions.worktreeConfig` and `core.bare` from that file do not count,
//! and `read_and_verify_repository_format()` (:766-769) verifies nothing.
//! Also the exact-name lookups `handle_extension()` (:653-716) makes:
//! `hash_algo_by_name()` (hash.c:331-339) and `ref_storage_format_by_name()`
//! (refs.c:49-55) compare with `strcmp`, and a `refStorage` value is a URI
//! whose format is the part before `://` (`parse_reference_uri()`, :635-648).
//!
//! Every case runs stock git and zvcs on twin copies of a stock-built
//! repository whose `.git/config` is replaced, and requires the same exit code,
//! stdout and stderr.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    stock: &'static str,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 46, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-repofmt-absent-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let f = Fixture { root, stock };
        for (dir, format) in [("F", "files"), ("R", "reftable")] {
            let ok = |out: Output| assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            ok(f.git(f.stock, &f.root, &["init", "-q", "-b", "main", &format!("--ref-format={format}"), dir]));
            ok(f.git(f.stock, &f.root.join(dir), &["commit", "-q", "--allow-empty", "-m", "one"]));
        }
        Some(f)
    }

    fn git(&self, bin: &str, dir: &Path, args: &[&str]) -> Output {
        Command::new(bin)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap()
    }

    /// Run `args` in a fresh copy of `base` whose config is `config`, once with
    /// stock and once with zvcs, and require identical results.
    fn assert_same(&self, base: &str, config: &str, args: &[&str]) {
        let mut results = Vec::new();
        for (who, bin) in [("stock", self.stock), ("zvcs", ZVCS)] {
            let dir = self.root.join(format!("{base}-{who}"));
            let _ = std::fs::remove_dir_all(&dir);
            let status = Command::new("cp").arg("-R").arg(self.root.join(base)).arg(&dir).status().unwrap();
            assert!(status.success());
            std::fs::write(dir.join(".git/config"), config).unwrap();
            let out = self.git(bin, &dir, args);
            let text = |b: &[u8]| String::from_utf8_lossy(b).replace(&format!("{base}-{who}"), "<dir>");
            results.push((out.status.code(), text(&out.stdout), text(&out.stderr)));
        }
        assert_eq!(results[1], results[0], "{base} with {config:?}: {args:?}");
    }
}

#[test]
fn extensions_without_a_version_are_ignored() {
    let Some(f) = Fixture::new("ignored") else { return };
    let show = ["rev-parse", "--show-object-format", "--show-ref-format", "HEAD"];
    f.assert_same("F", "[extensions]\n\tobjectFormat = sha256\n", &show);
    f.assert_same("F", "[extensions]\n\trefStorage = reftable\n", &show);
    f.assert_same("F", "[extensions]\n\tobjectFormat = sha256\n", &["for-each-ref"]);
    // The repository declared reftable and lost its version: it is read as a
    // files repository, never through the stack.
    f.assert_same(
        "R",
        "[extensions]\n\trefStorage = reftable\n\tobjectFormat = sha256\n",
        &["rev-parse", "--show-object-format", "--show-ref-format"],
    );
}

#[test]
fn core_bare_without_a_version_is_ignored_by_setup() {
    let Some(f) = Fixture::new("bare") else { return };
    f.assert_same(
        "F",
        "[core]\n\tbare = true\n",
        &["rev-parse", "--is-bare-repository", "--is-inside-work-tree"],
    );
    f.assert_same(
        "F",
        "[core]\n\trepositoryformatversion = 0\n\tbare = true\n",
        &["rev-parse", "--is-bare-repository"],
    );
}

#[test]
fn extension_values_are_matched_exactly() {
    let Some(f) = Fixture::new("exact") else { return };
    let v1 = "[core]\n\trepositoryformatversion = 1\n[extensions]\n";
    f.assert_same("F", &format!("{v1}\tobjectFormat = SHA1\n"), &["rev-parse", "--show-object-format"]);
    f.assert_same("F", &format!("{v1}\tobjectFormat = sha1\n"), &["rev-parse", "--show-object-format"]);
    f.assert_same("R", &format!("{v1}\trefStorage = reftable\n"), &["rev-parse", "--show-ref-format", "HEAD"]);
    f.assert_same("R", &format!("{v1}\trefStorage = files\n"), &["rev-parse", "--show-ref-format"]);
}
