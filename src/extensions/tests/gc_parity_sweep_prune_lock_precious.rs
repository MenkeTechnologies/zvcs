//! `gc`, `prune` and `repack` corners found by a differential sweep against
//! stock git 2.56.0.
//!
//!   * `odb_optimize()` runs `prune --expire <expiry>` after the repack for every
//!     expiry but `--no-prune`'s (odb/source-files.c:734-753); zvcs only pruned
//!     under `--prune=now`, so an unreachable loose object older than
//!     `2.weeks.ago` and a stale `tmp_*` file survived a plain `gc`.
//!   * `lock_repo_for_gc()` (builtin/gc.c:419-483): a live `gc.pid` on this
//!     host, or any recent one from another host, stops a manual `gc` unless
//!     `--force`; the pid file goes away at exit, and a run that completes
//!     removes `gc.log` (builtin/gc.c:741-745). zvcs took no lock.
//!   * `extensions.preciousObjects`: `prune` dies, `repack -d` dies, and `gc`
//!     skips the object store entirely (builtin/prune.c:179-180,
//!     builtin/repack.c:397-398, odb/source-files.c:620-621).
//!   * `prune` names the object directory `./objects` when run from the git
//!     directory itself (setup.c:1303-1311), and an object fan-out entry that
//!     is not a directory ends the recent-object scan with
//!     `unable to mark recent objects` (object-file.c:1062-1067,
//!     reachable.c:349-351).

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

fn backdate(path: &Path) {
    let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_577_836_800);
    let file = std::fs::File::open(path).unwrap();
    file.set_modified(old).unwrap();
}

impl Fixture {
    /// A packed commit, a second loose one, an unreachable blob backdated to
    /// 2020 and a backdated stale `objects/pack/tmp_pack_x`. Built with stock git.
    fn new(stock: &str, tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-gc-sweep-{tag}-{}", std::process::id()));
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
        std::fs::write(seed.join("a"), "a\n").unwrap();
        g(&["add", "a"]);
        g(&["commit", "-q", "-m", "a"]);
        g(&["repack", "-q", "-d"]);
        std::fs::write(seed.join("b"), "b\n").unwrap();
        g(&["add", "b"]);
        g(&["commit", "-q", "-m", "b"]);
        let out = {
            let child = Command::new(stock)
                .args(["hash-object", "-w", "--stdin"])
                .current_dir(&seed)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            use std::io::Write as _;
            child.stdin.as_ref().unwrap().write_all(b"old unreachable\n").unwrap();
            let out = child.wait_with_output().unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        let id = out.trim();
        backdate(&seed.join(".git/objects").join(&id[..2]).join(&id[2..]));
        let tmp = seed.join(".git/objects/pack/tmp_pack_x");
        std::fs::write(&tmp, "stale\n").unwrap();
        backdate(&tmp);
        f
    }

    /// A fresh copy of the seed repository for one binary.
    fn twin(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        let _ = std::fs::remove_dir_all(&dir);
        copy_dir(&self.root.join("seed"), &dir);
        dir
    }

    /// Run `args` in a fresh twin per binary (after `prepare`), and require the
    /// same output and the same object store afterwards.
    fn same(&self, stock: &str, sub: &str, prepare: &dyn Fn(&Path), args: &[&str]) {
        let mut seen = Vec::new();
        for (name, bin) in [("stock", stock), ("zvcs", BIN)] {
            let dir = self.twin(name);
            prepare(&dir);
            let out = git(bin, &dir.join(sub), &self.root, args);
            let state = self.state(stock, &dir);
            seen.push((out, state));
        }
        assert_eq!(seen[1].0, seen[0].0, "output of {args:?}");
        assert_eq!(seen[1].1, seen[0].1, "object store after {args:?}");
    }

    /// Loose objects, packs and the gc bookkeeping files, as stock sees them.
    fn state(&self, stock: &str, dir: &Path) -> String {
        let (count, _, _) = git(stock, dir, &self.root, &["count-objects", "-v"]);
        let count: String = count.lines().filter(|l| !l.starts_with("size")).collect::<Vec<_>>().join("\n");
        let mut files: Vec<String> = ["gc.pid", "gc.log", "objects/pack/tmp_pack_x"]
            .iter()
            .filter(|f| dir.join(".git").join(f).exists())
            .map(|f| f.to_string())
            .collect();
        files.sort();
        format!("{count}\n{files:?}")
    }
}

fn nothing(_: &Path) {}

#[test]
fn gc_prunes_old_unreachable_objects_and_prune_paths() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "prune");
    f.same(stock, "", &nothing, &["-c", "maintenance.auto=false", "gc", "-q"]);
    f.same(stock, ".git", &nothing, &["prune", "-n", "--expire=now"]);
    f.same(
        stock,
        "",
        &|dir: &Path| std::fs::write(dir.join(".git/objects/ab"), "x\n").unwrap(),
        &["prune", "-v", "--expire=now"],
    );
}

#[test]
fn gc_pid_lock_and_gc_log() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "lock");
    let host = {
        let mut buf = [0u8; 257];
        // SAFETY: the buffer is valid for its whole length.
        assert_eq!(unsafe { libc_gethostname(buf.as_mut_ptr(), buf.len()) }, 0);
        let end = buf.iter().position(|&b| b == 0).unwrap();
        String::from_utf8_lossy(&buf[..end]).into_owned()
    };
    // This test process is alive, so its pid on this host is a running gc.
    let live = format!("{} {host}", std::process::id());
    let write = |body: String| move |dir: &Path| std::fs::write(dir.join(".git/gc.pid"), &body).unwrap();
    let gc = ["-c", "maintenance.auto=false", "gc", "-q"];
    // The message names the pid and host; only the exit code and the store
    // are compared for a held lock, since the line is the same for both.
    f.same(stock, "", &write("999999 some-other-host".into()), &gc);
    f.same(stock, "", &write(live), &gc);
    // A dead pid on this host is no lock: the run goes ahead, takes `gc.pid`,
    // removes it at exit and clears the `gc.log` a failed run left.
    let stale = format!("999999 {host}");
    f.same(stock, "", &move |dir: &Path| {
        std::fs::write(dir.join(".git/gc.pid"), &stale).unwrap();
        std::fs::write(dir.join(".git/gc.log"), "boom\n").unwrap();
    }, &gc);
}

extern "C" {
    #[link_name = "gethostname"]
    fn libc_gethostname(name: *mut u8, len: usize) -> i32;
}

#[test]
fn precious_objects_refuse_pruning() {
    let Some(stock) = stock_git() else { return };
    let f = Fixture::new(stock, "precious");
    let precious = |dir: &Path| {
        let config = dir.join(".git/config");
        let mut text = std::fs::read_to_string(&config).unwrap();
        text = text.replace("repositoryformatversion = 0", "repositoryformatversion = 1");
        text.push_str("[extensions]\n\tpreciousObjects = true\n");
        std::fs::write(&config, text).unwrap();
    };
    f.same(stock, "", &precious, &["prune", "-n"]);
    f.same(stock, "", &precious, &["repack", "-a", "-d", "-q"]);
    f.same(stock, "", &precious, &["-c", "maintenance.auto=false", "gc", "-q"]);
}
