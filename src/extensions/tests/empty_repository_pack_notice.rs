//! `Nothing new to pack.` in a repository that holds no objects at all.
//!
//! The notice belongs to `repack`, which prints it whenever its `pack-objects`
//! child wrote no pack:
//!
//! ```c
//! if (!names.nr) {
//!         if (!po_args.quiet)
//!                 printf_ln(_("Nothing new to pack."));
//! ```
//!
//! (`builtin/repack.c:460-462`.) The gate is `!names.nr` and `-q`, and nothing
//! else — in particular `-a` is not an exemption. `-a` normally has the whole
//! object store to pack and so never reaches it, which is exactly why an *empty*
//! store is the case that separates "`-a` never gets here" from "`-a` is
//! excluded": there is nothing to write however total the repack is.
//!
//! `gc` inherits the line rather than printing one of its own: it runs
//! `repack -d -l` as a child (`builtin/gc.c:897`, with `-a`/`-A`/`--cruft`
//! appended by `add_repack_all_option()` and `-q` by `builtin/gc.c:926-927`), so
//! the child's stdout is `gc`'s stdout.
//!
//! Every expectation below was captured from stock git 2.55.0 in a repository
//! made by nothing but `git init`:
//!
//! ```console
//! $ git init --bare b && cd b
//! $ git gc          # -> "Nothing new to pack.\n" on stdout, exit 0
//! $ git repack -ad  # -> "Nothing new to pack.\n" on stdout, exit 0
//! $ git repack -adq # -> nothing, exit 0
//! ```
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const NOTICE: &str = "Nothing new to pack.\n";

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-packnotice-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
            .env("ZVCS_HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_SYSTEM", self.root.join("gitconfig-system"))
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run binary")
    }

    /// An object store with nothing in it: no commit, no index, no pack.
    fn empty_repo(&self, name: &str, bare: bool) -> PathBuf {
        let dir = self.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut args = vec!["init", "-q", "-b", "main"];
        if bare {
            args.push("--bare");
        }
        args.push(".");
        let out = self.run(&dir, &args);
        assert!(out.status.success(), "init failed: {out:?}");
        assert!(
            std::fs::read_dir(dir.join(if bare { "objects/pack" } else { ".git/objects/pack" }))
                .unwrap()
                .next()
                .is_none(),
            "fixture is not object-free"
        );
        dir
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn repack_says_so_with_and_without_all_into_one() {
    let fx = Fixture::new("repack");

    // `-a`, `-A` and `--cruft` all set `all_into_one`, and every one of them
    // still reaches the notice here. `repack -d` alone is the case that already
    // worked; it is kept so a regression cannot be mistaken for a mode-specific
    // quirk.
    for (bare, args) in [
        (true, &["repack", "-ad"][..]),
        (true, &["repack", "-Ad"][..]),
        (true, &["repack", "-d", "--cruft"][..]),
        (true, &["repack", "-d"][..]),
        (true, &["repack", "-a"][..]),
        (false, &["repack", "-ad"][..]),
        (false, &["repack", "-d"][..]),
    ] {
        let dir = fx.empty_repo(&format!("r{}{}", usize::from(bare), args.join("")), bare);
        let out = fx.run(&dir, args);
        assert_eq!(out.status.code(), Some(0), "git {args:?} exit: {out:?}");
        assert_eq!(stdout(&out), NOTICE, "git {args:?} stdout");
        assert_eq!(stderr(&out), "", "git {args:?} stderr");
    }
}

#[test]
fn gc_relays_the_notice_from_its_repack() {
    let fx = Fixture::new("gc");

    for (bare, args) in [
        (true, &["gc"][..]),
        (true, &["gc", "--no-cruft"][..]),
        (false, &["gc"][..]),
    ] {
        let dir = fx.empty_repo(&format!("g{}{}", usize::from(bare), args.join("")), bare);
        let out = fx.run(&dir, args);
        assert_eq!(out.status.code(), Some(0), "git {args:?} exit: {out:?}");
        assert_eq!(stdout(&out), NOTICE, "git {args:?} stdout");
    }
}

#[test]
fn quiet_suppresses_only_the_notice() {
    let fx = Fixture::new("quiet");

    // `po_args.quiet` is what gates it, so both spellings of the flag silence it
    // — and silence nothing else: the exit code is unchanged and stderr stays
    // empty, because the notice is the whole of the output.
    for args in [
        &["repack", "-adq"][..],
        &["repack", "-ad", "--quiet"][..],
        &["repack", "-dq"][..],
        &["gc", "-q"][..],
        &["gc", "--quiet"][..],
    ] {
        let dir = fx.empty_repo(&format!("q{}", args.join("")), true);
        let out = fx.run(&dir, args);
        assert_eq!(out.status.code(), Some(0), "git {args:?} exit: {out:?}");
        assert_eq!(stdout(&out), "", "git {args:?} stdout");
        assert_eq!(stderr(&out), "", "git {args:?} stderr");
    }
}

#[test]
fn a_repository_with_objects_does_not_say_so() {
    let fx = Fixture::new("nonempty");
    let dir = fx.empty_repo("work", false);

    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    for args in [
        &["-c", "user.email=t@e.co", "-c", "user.name=t", "add", "a.txt"][..],
        &["-c", "user.email=t@e.co", "-c", "user.name=t", "commit", "-q", "-m", "one"][..],
    ] {
        let out = fx.run(&dir, args);
        assert!(out.status.success(), "setup git {args:?}: {out:?}");
    }

    // `-a` has the commit, its tree and its blob to write, so `names.nr` is not
    // zero and the notice never fires. This is the guard that keeps the fix from
    // degenerating into "always print it".
    let out = fx.run(&dir, &["repack", "-ad"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stdout(&out), "", "repack -ad on a populated store: {out:?}");

    // A second `-d` run has nothing left loose to add, so this one does say it.
    let out = fx.run(&dir, &["repack", "-d"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stdout(&out), NOTICE, "{out:?}");
}
