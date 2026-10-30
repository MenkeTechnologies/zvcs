//! `git stash export` and `git stash import` (builtin/stash.c:2075-2454).
//!
//! `export` writes a parentless base commit over the empty tree under the fixed
//! ident `git stash <git@stash> 1000684800 +0000`, then one copy of every stash
//! commit whose first parent chains to the previous copy and whose second parent
//! is the stash itself, keeping its author and committer and prefixing its
//! message with `git stash: `. `import` walks that chain back to the base,
//! checking every link, and stores the second parents in `refs/stash` oldest
//! first. zvcs parsed the options of both and then bailed.
//!
//! Each case builds one repository with stock git, copies it, and runs the
//! command under both binaries with identities and dates pinned; the exported ids
//! are compared byte for byte, and so is the stock-read post-state.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

struct Fixture {
    root: PathBuf,
    stock: &'static str,
    n: std::cell::Cell<usize>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Exit status, stdout and stderr, plus the stock-read post-state.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    code: i32,
    stdout: String,
    stderr: String,
    refs: String,
    stash_log: String,
    exp_log: String,
    fsck: String,
}

impl Fixture {
    /// `main` at `init`, and three stashes: a worktree change, one with an
    /// untracked file and a message, and one with a staged and an unstaged change.
    /// `refs/bad/*` hold hand-made commits that are not valid exports.
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 56, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-stash-export-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("seed")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root, stock, n: std::cell::Cell::new(0) };
        let seed = f.root.join("seed");
        let git = |args: &[&str]| f.stock_ok(&seed, args);
        git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(seed.join("f"), "a\n").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", "init"]);
        std::fs::write(seed.join("f"), "a\nb\n").unwrap();
        git(&["stash", "-q"]);
        std::fs::write(seed.join("f"), "a\nc\n").unwrap();
        std::fs::write(seed.join("u"), "u\n").unwrap();
        git(&["stash", "-q", "-u", "-m", "with untracked"]);
        std::fs::write(seed.join("f"), "a\nd\n").unwrap();
        git(&["add", "f"]);
        std::fs::write(seed.join("f"), "a\nd\ne\n").unwrap();
        git(&["stash", "-q"]);

        let base = git(&["stash", "export", "--print", "0"]);
        let base = git(&["rev-parse", &format!("{}^1", base.trim())]);
        let base = base.trim();
        let stash = git(&["rev-parse", "stash@{0}"]);
        let stash = stash.trim();
        let commit = |args: &[&str]| git(&[&["commit-tree"][..], args].concat()).trim().to_string();
        let bad = [
            ("one-parent", commit(&["-p", "HEAD", EMPTY_TREE, "-m", "git stash: x"])),
            ("bad-root", commit(&[EMPTY_TREE, "-m", "x"])),
            ("no-prefix", commit(&["-p", base, "-p", stash, EMPTY_TREE, "-m", "x"])),
            ("not-stash", commit(&["-p", base, "-p", "HEAD", EMPTY_TREE, "-m", "git stash: x"])),
            ("three", commit(&["-p", base, "-p", stash, "-p", "HEAD", EMPTY_TREE, "-m", "git stash: x"])),
            ("tree", commit(&["-p", base, "-p", stash, "HEAD^{tree}", "-m", "git stash: x"])),
            ("handmade", commit(&["-p", base, "-p", stash, EMPTY_TREE, "-m", "git stash: own\n\nbody"])),
        ];
        for (name, id) in &bad {
            git(&["update-ref", &format!("refs/bad/{name}"), id]);
        }
        // A root with no ident lines: `do_import_stash()` reports it and returns 0.
        let raw = seed.join("raw");
        std::fs::write(&raw, format!("tree {EMPTY_TREE}\n\nx")).unwrap();
        let id = git(&["hash-object", "-t", "commit", "-w", "--literally", "raw"]);
        git(&["update-ref", "refs/bad/no-author", id.trim()]);
        std::fs::remove_file(raw).unwrap();
        Some(f)
    }

    /// A command with a pinned identity and date.
    fn cmd(&self, bin: &str, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(bin);
        c.args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("ZVCS_HOME", self.root.join(".zvcs"))
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC");
        c
    }

    fn run_ok(&self, bin: &str, dir: &Path, args: &[&str]) -> String {
        let out = self.cmd(bin, dir, args).output().unwrap();
        assert!(out.status.success(), "{bin} {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn stock_ok(&self, dir: &Path, args: &[&str]) -> String {
        self.run_ok(self.stock, dir, args)
    }

    fn stock_out(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.cmd(self.stock, dir, args).output().unwrap();
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        s
    }

    /// A fresh copy of the seed with the stock `setup` commands run in it.
    fn copy(&self, label: &str, setup: &[&[&str]]) -> PathBuf {
        let n = self.n.get();
        self.n.set(n + 1);
        let dir = self.root.join(format!("{label}-{n}"));
        let cp = Command::new("/bin/cp").arg("-Rp").arg(self.root.join("seed")).arg(&dir).status().unwrap();
        assert!(cp.success());
        for args in setup {
            self.stock_ok(&dir, args);
        }
        dir
    }

    fn state(&self, dir: &Path, code: i32, stdout: &[u8], stderr: &[u8]) -> Outcome {
        let root = self.root.to_string_lossy().into_owned();
        let clean = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<root>");
        let log = |name: &str| {
            self.stock_out(dir, &["log", "-g", "--format=%H %P %gn <%ge> %gd %gs", name, "--"])
        };
        Outcome {
            code,
            stdout: clean(stdout),
            stderr: clean(stderr),
            refs: self.stock_out(dir, &["for-each-ref"]),
            stash_log: log("refs/stash"),
            exp_log: log("refs/heads/exp"),
            fsck: self.stock_out(dir, &["fsck", "--no-progress", "--no-dangling"]),
        }
    }

    /// Run `args` under `bin` after `setup`, with the variables in `unset` removed.
    fn outcome(&self, bin: &str, setup: &[&[&str]], unset: &[&str], args: &[&str]) -> Outcome {
        let dir = self.copy(if bin == ZVCS { "zvcs" } else { "stock" }, setup);
        let mut c = self.cmd(bin, &dir, args);
        for k in unset {
            c.env_remove(k);
        }
        let out = c.output().unwrap();
        self.state(&dir, out.status.code().expect("no signal"), &out.stdout, &out.stderr)
    }

    #[track_caller]
    fn same_with(&self, setup: &[&[&str]], unset: &[&str], args: &[&str]) -> Outcome {
        let stock = self.outcome(self.stock, setup, unset, args);
        let zvcs = self.outcome(ZVCS, setup, unset, args);
        assert_eq!(zvcs, stock, "git {args:?} after {setup:?}: left is zvcs, right is stock");
        stock
    }

    #[track_caller]
    fn same(&self, args: &[&str]) -> Outcome {
        self.same_with(&[], &[], args)
    }
}

/// The exported chain is byte-identical: the base commit, every copy and its
/// message, whether printed, written to a ref (through `HEAD`, too), or built
/// from named entries in the order given.
#[test]
fn export_writes_the_same_chain() {
    let Some(f) = Fixture::new("export") else { return };
    let got = f.same(&["stash", "export", "--print"]);
    assert_eq!(got.code, 0);
    assert_eq!(got.stdout.trim().len(), 40);
    f.same(&["stash", "export", "--to-ref", "refs/heads/exp"]);
    f.same(&["stash", "export", "--to-ref=refs/exp/x", "1", "stash@{2}"]);
    f.same(&["stash", "export", "--print", "2", "0", "1"]);
    f.same(&["stash", "export", "--print", "0", "0"]);
    f.same(&["stash", "export", "--to-ref", "HEAD"]);
    f.same(&["stash", "export", "--to-ref", "x", "--no-to-ref", "--print"]);
    f.same(&["-c", "i18n.commitEncoding=ISO-8859-1", "stash", "export", "--print"]);
    // No stash at all is the base commit alone.
    let got = f.same_with(&[&["stash", "clear"]], &[], &["stash", "export", "--print"]);
    assert_eq!(got.code, 0);
}

/// `prepare_fallback_ident("git stash", "git@stash")` (builtin/stash.c:2337) names
/// the `--to-ref` reflog entry when no committer is given; the commits themselves
/// never depend on the identity.
#[test]
fn export_to_ref_logs_under_the_fallback() {
    let Some(f) = Fixture::new("fallback") else { return };
    let args = ["stash", "export", "--to-ref", "refs/heads/exp"];
    let got = f.same_with(&[], &["GIT_COMMITTER_NAME", "GIT_COMMITTER_EMAIL"], &args);
    assert!(got.exp_log.contains("git stash <git@stash>"), "{}", got.exp_log);
    f.same_with(&[], &["GIT_COMMITTER_NAME"], &args);
}

/// The refusals: the mode check, an entry that does not resolve (past the end, at
/// the end, older than the log), something that is not a stash, a reflog entry
/// that is not a stash (reported, yet exit 0), and a ref `update_ref()` refuses.
#[test]
fn export_refuses_as_stock_does() {
    let Some(f) = Fixture::new("export-err") else { return };
    for args in [
        &["stash", "export"][..],
        &["stash", "export", "--print", "--to-ref", "x"],
        &["stash", "export", "--print", "7"],
        &["stash", "export", "--print", "3"],
        &["stash", "export", "--print", "stash@{3}"],
        &["stash", "export", "--print", "stash@{2000-01-01}"],
        &["stash", "export", "--print", "nope"],
        &["stash", "export", "--print", ""],
        &["stash", "export", "--print", "--", "0"],
        &["stash", "export", "--print", "HEAD"],
        &["stash", "export", "--print", "HEAD:f"],
        &["stash", "export", "--print", "abcdef7890123456789012345678901234567890"],
        &["stash", "export", "--to-ref", "bad..x"],
        &["stash", "export", "--to-ref", "refs/heads/main/sub"],
    ] {
        let got = f.same(args);
        assert_ne!(got.code, 0, "{args:?}");
    }
    let not_a_stash: &[&[&str]] = &[&["update-ref", "-m", "plain", "refs/stash", "HEAD"]];
    let got = f.same_with(not_a_stash, &[], &["stash", "export", "--print"]);
    assert_eq!(got.code, 0);
    assert!(got.stdout.is_empty());
    f.same_with(not_a_stash, &[], &["stash", "export", "--print", "1"]);
}

/// `import` restores every entry, oldest first, under the stash commits' own
/// messages; an entry already at the tip of `refs/stash` logs nothing, and a tag
/// over the chain is peeled.
#[test]
fn import_restores_the_entries() {
    let Some(f) = Fixture::new("import") else { return };
    let export: &[&str] = &["stash", "export", "--to-ref", "refs/exp/all"];
    let got = f.same_with(&[export, &["stash", "clear"]], &[], &["stash", "import", "refs/exp/all"]);
    assert_eq!(got.code, 0);
    assert_eq!(got.stash_log.lines().count(), 3, "{}", got.stash_log);
    f.same_with(&[export], &[], &["stash", "import", "refs/exp/all"]);
    f.same_with(
        &[&["stash", "export", "--to-ref", "refs/exp/all", "1", "0"], &["stash", "drop", "-q"]],
        &[],
        &["stash", "import", "refs/exp/all"],
    );
    f.same_with(
        &[export, &["stash", "clear"], &["tag", "-a", "-m", "t", "tg", "refs/exp/all"]],
        &[],
        &["stash", "import", "tg"],
    );
    f.same_with(&[export, &["stash", "clear"]], &["GIT_COMMITTER_NAME"], &["stash", "import", "refs/exp/all"]);
    f.same_with(&[&["stash", "clear"]], &[], &["stash", "import", "refs/bad/handmade"]);
}

/// A chain one binary exports, the other imports, in both directions.
#[test]
fn export_and_import_interoperate() {
    let Some(f) = Fixture::new("cross") else { return };
    let mut stashes = Vec::new();
    for (exporter, importer) in [(ZVCS, f.stock), (f.stock, ZVCS), (ZVCS, ZVCS)] {
        let dir = f.copy("cross", &[]);
        let id = f.run_ok(exporter, &dir, &["stash", "export", "--print"]);
        f.stock_ok(&dir, &["stash", "clear"]);
        f.run_ok(importer, &dir, &["stash", "import", id.trim()]);
        stashes.push((id, f.stock_out(&dir, &["log", "-g", "--format=%H %gs", "refs/stash", "--"])));
    }
    let dir = f.copy("cross", &[]);
    let id = f.stock_ok(&dir, &["stash", "export", "--print"]);
    let original = f.stock_out(&dir, &["log", "-g", "--format=%H %gs", "refs/stash", "--"]);
    for got in &stashes {
        assert_eq!(got, &(id.clone(), original.clone()));
    }
}

/// Every refusal `do_import_stash()` has for bad input, the base commit alone (an
/// empty chain), the root without idents it reports and still exits 0 for, and the
/// usage refusal.
#[test]
fn import_refuses_as_stock_does() {
    let Some(f) = Fixture::new("import-err") else { return };
    let base = f.stock_ok(&f.root.join("seed"), &["rev-parse", "refs/bad/no-prefix^1"]);
    for rev in [
        "HEAD",
        "HEAD:f",
        "HEAD^{tree}",
        "nope",
        "abcdef7890123456789012345678901234567890",
        "refs/bad/one-parent",
        "refs/bad/bad-root",
        "refs/bad/no-prefix",
        "refs/bad/not-stash",
        "refs/bad/three",
        "refs/bad/tree",
    ] {
        let got = f.same(&["stash", "import", rev]);
        assert_ne!(got.code, 0, "{rev}");
    }
    // The base alone is a chain with nothing on it: nothing is stored.
    let got = f.same(&["stash", "import", base.trim()]);
    assert_eq!(got.code, 0);
    let got = f.same(&["stash", "import", "refs/bad/no-author"]);
    assert_eq!(got.code, 0);
    assert!(got.stderr.starts_with("error: cannot parse commit"), "{}", got.stderr);
    for args in [&["stash", "import"][..], &["stash", "import", "a", "b"], &["stash", "import", "--", "x"]] {
        let got = f.same(args);
        assert_eq!(got.code, 129);
    }
}
