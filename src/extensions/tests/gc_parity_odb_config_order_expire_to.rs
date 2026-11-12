//! `gc` and `maintenance` against stock git 2.56.0 where 2.56 moved config
//! reads out of `gc_config()`, and `gc --expire-to`.
//!
//!   * `gc.auto`, `gc.autoPackLimit` and `gc.bigPackThreshold` are read by
//!     `odb_source_files_optimize()` (odb/source-files.c:616-618) and
//!     `gc.repackFilter` by `add_repack_all_option()` (:484-485), after
//!     `pack-refs` and `reflog expire` ran; zvcs validated them up front in
//!     2.55's `gc_config()` order and left the refs and reflogs untouched.
//!   * `core.deltaBaseCacheLimit` is a repository setting the `pack-refs` and
//!     `worktree prune` children die on, each reported by `gc`;
//!     `pack.deltaCacheSize` is the `pack-objects` grandchild's.
//!   * `maintenance run --task=gc` reports the `gc` child's death as a failed
//!     task, and `loose-objects` never reads `gc.auto` at all.
//!   * `gc --prune=now --expire-to=<prefix>` writes the limbo cruft pack
//!     `<prefix>-<hash>.{idx,mtimes,pack,rev}` (builtin/repack.c:654-689);
//!     zvcs wrote none.

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

fn git(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_DIR")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
            let mtime = std::fs::metadata(entry.path()).unwrap().modified().unwrap();
            std::fs::File::open(&target).unwrap().set_modified(mtime).unwrap();
        }
    }
}

/// Every file under `dir` dated 2026-01-01, so `.mtimes` contents are fixed.
fn pin_mtimes(dir: &Path) {
    let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_767_225_600);
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            pin_mtimes(&entry.path());
        } else {
            std::fs::File::open(entry.path()).unwrap().set_modified(when).unwrap();
        }
    }
}

impl Fixture {
    /// Three loose commits on `main`, a `side` branch and a tag as loose refs, a
    /// reflog whose pinned 2023 dates expire, an amended-away commit and an
    /// unreachable blob. Built with stock git.
    fn new(stock: &str, tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-gc-odb-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        let f = Fixture { root };
        let g = |args: &[&str]| {
            let mut all = vec!["-c", "maintenance.auto=false", "-c", "gc.auto=0"];
            all.extend_from_slice(args);
            let (_, err, code) = git(stock, &seed, &f.root, &all);
            assert_eq!(code, Some(0), "{args:?}: {err}");
        };
        g(&["init", "-q", "-b", "main"]);
        for name in ["f1", "f2", "f3"] {
            std::fs::write(seed.join(name), format!("{name}\n")).unwrap();
            g(&["add", name]);
            g(&["commit", "-q", "-m", name]);
        }
        g(&["branch", "side", "HEAD~1"]);
        g(&["tag", "t1", "HEAD~2"]);
        std::fs::write(seed.join("loose"), "unreachable\n").unwrap();
        g(&["hash-object", "-w", "loose"]);
        std::fs::remove_file(seed.join("loose")).unwrap();
        g(&["commit", "-q", "--amend", "-m", "amended"]);
        pin_mtimes(&seed.join(".git/objects"));
        f
    }

    /// A fresh copy of the seed for one binary.
    fn copy(&self, tag: &str) -> PathBuf {
        let dir = self.root.join(tag);
        let _ = std::fs::remove_dir_all(&dir);
        copy_dir(&self.root.join("seed"), &dir);
        dir
    }
}

/// What the run left behind, read with stock git: the refs, which of them are
/// still loose, whether `packed-refs` was written and how many `main` reflog
/// entries survive.
fn post_state(stock: &str, dir: &Path, home: &Path) -> String {
    let refs = git(stock, dir, home, &["for-each-ref"]).0;
    let reflog = git(stock, dir, home, &["reflog", "show", "main"]).0;
    let mut loose: Vec<String> = Vec::new();
    for sub in ["heads", "tags"] {
        if let Ok(entries) = std::fs::read_dir(dir.join(".git/refs").join(sub)) {
            for entry in entries.flatten() {
                loose.push(format!("{sub}/{}", entry.file_name().to_string_lossy()));
            }
        }
    }
    loose.sort();
    format!(
        "{refs}loose={loose:?} packed-refs={} reflog={}",
        dir.join(".git/packed-refs").exists(),
        reflog.lines().count()
    )
}

/// Run `args` through both binaries on copies of the seed and compare stdout,
/// stderr, the exit code and the post-state.
fn assert_same(f: &Fixture, stock: &str, args: &[&str]) {
    let s = f.copy("stock");
    let z = f.copy("zvcs");
    let want = git(stock, &s, &f.root, args);
    let got = git(BIN, &z, &f.root, args);
    assert_eq!(got, want, "{args:?}");
    assert_eq!(post_state(stock, &z, &f.root), post_state(stock, &s, &f.root), "{args:?}");
}

#[test]
fn config_gc_moved_out_of_gc_config_dies_after_the_foreground_tasks() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(&stock, "config");
    for args in [
        // odb/source-files.c:616-618, after pack-refs and reflog expire.
        &["-c", "gc.auto=abc", "gc"][..],
        &["-c", "gc.bigPackThreshold=abc", "gc"],
        // :484-485, `config_error_nonbool()` then `git_die_config()`.
        &["-c", "gc.repackFilter", "gc"],
        // `--auto` reads `gc.auto` first, in the threshold check.
        &["-c", "gc.auto=abc", "gc", "--auto"],
        // `gc.auto=0` returns before the pack walk that needs the settings.
        &["-c", "core.deltaBaseCacheLimit=abc", "-c", "gc.auto=0", "gc", "--auto"],
        // The `pack-refs` and `worktree prune` children each die on it.
        &["-c", "core.deltaBaseCacheLimit=abc", "gc"],
        // The `pack-objects` grandchild's `git_pack_config()`.
        &["-c", "pack.deltaCacheSize=abc", "gc"],
    ] {
        assert_same(&f, &stock, args);
    }
}

#[test]
fn maintenance_reads_gc_auto_only_where_stock_does() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(&stock, "maintenance");
    for args in [
        // The `gc --skip-foreground-tasks` child dies; the task fails, exit 1.
        &["-c", "gc.auto=abc", "maintenance", "run", "--task=gc"][..],
        // Nothing in `loose-objects` reads `gc.auto`.
        &["-c", "gc.auto=abc", "maintenance", "run", "--task=loose-objects", "--quiet"],
        &["-c", "gc.auto=abc", "maintenance", "is-needed"],
    ] {
        assert_same(&f, &stock, args);
    }
}

/// The `<prefix>-*` files `dir` holds, and the object names each `.idx` lists.
fn expired_packs(stock: &str, dir: &Path, home: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        if name.ends_with(".idx") {
            let idx = dir.join(&name);
            let listing = git(stock, dir, home, &["verify-pack", "-v", idx.to_str().unwrap()]).0;
            let mut ids: Vec<&str> = listing
                .lines()
                .filter_map(|l| l.split_whitespace().next().filter(|w| w.len() == 40))
                .collect();
            ids.sort();
            found.push(format!("{name}: {ids:?}"));
        }
        if name.ends_with(".mtimes") {
            found.push(format!("{name}: {:?}", std::fs::read(dir.join(&name)).unwrap()));
        }
        found.push(name);
    }
    found
}

#[test]
fn gc_expire_to_writes_the_limbo_cruft_pack() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(&stock, "expire-to");
    let s = f.copy("stock");
    let z = f.copy("zvcs");
    let (s_out, z_out) = (f.root.join("expired-stock"), f.root.join("expired-zvcs"));
    std::fs::create_dir_all(&s_out).unwrap();
    std::fs::create_dir_all(&z_out).unwrap();
    let run = |bin: &str, dir: &Path, out: &Path| {
        let prefix = format!("--expire-to={}", out.join("x").display());
        git(bin, dir, &f.root, &["gc", "--prune=now", &prefix])
    };
    assert_eq!(run(BIN, &z, &z_out), run(&stock, &s, &s_out));
    let want = expired_packs(&stock, &s_out, &f.root);
    assert!(want.iter().any(|n| n.ends_with(".mtimes")), "{want:?}");
    assert_eq!(expired_packs(&stock, &z_out, &f.root), want);
    assert_eq!(post_state(&stock, &z, &f.root), post_state(&stock, &s, &f.root));

    // A prefix whose directory is missing: `pack-objects` cannot move the pack
    // there, and `gc` reports the `repack` child's death.
    let s = f.copy("stock");
    let z = f.copy("zvcs");
    let missing = |out: &str| format!("--expire-to={}", f.root.join(out).join("x").display());
    let want = git(&stock, &s, &f.root, &["gc", "--prune=now", &missing("none")]);
    let got = git(BIN, &z, &f.root, &["gc", "--prune=now", &missing("none")]);
    assert_eq!(got, want);
}
