//! Three measured divergences against stock git 2.55.0, all about *ordering* —
//! what is already on disk at the moment a command gives up.
//!
//! 1. `sparse_checkout_set()` runs `update_modes()` before it ever looks at the
//!    arguments:
//!
//! ```c
//!     if (update_modes(repo, &set_opts.cone_mode, &set_opts.sparse_index))
//!             return 1;
//!     …
//!     } else {
//!             for (int i = 0; i < argc; i++)
//!                     strvec_push(&patterns, argv[i]);
//!             sanitize_paths(repo, &patterns, prefix, set_opts.skip_checks);
//!     }
//! ```
//!
//!    (builtin/sparse-checkout.c:875-890, with `update_modes()` at :418-444 and
//!    `set_config()` at :377-399.) So a definition that `sanitize_paths()` dies
//!    on still leaves `core.sparseCheckout=true`, `core.sparseCheckoutCone` and
//!    the `extensions.worktreeConfig` that `init_worktree_config()` turns on.
//!    The port wrote the config only after the patterns had been vetted, so a
//!    rejected `set` left the repository untouched.
//!
//!    Measured, stock git 2.55.0, fresh non-sparse repo:
//!
//! ```text
//!     $ git sparse-checkout set /folder1
//!     fatal: specify directories rather than patterns (no leading slash)
//!     $ echo $?
//!     128
//!     $ git sparse-checkout list
//!     warning: this worktree is not sparse (sparse-checkout file may not exist)
//!     $ echo $?
//!     0
//! ```
//!
//!    The port answered `fatal: this worktree is not sparse` / 128 to that
//!    `list`, because `sparse_checkout_list()`'s `die()` guard reads
//!    `cfg->apply_sparse_checkout` (builtin/sparse-checkout.c:67-68) and the
//!    config had never been written.
//!
//! 2. Both of `add`'s arms load the existing definition and die when it cannot
//!    be read (builtin/sparse-checkout.c:651-653 and :679-681, with the -1 from
//!    dir.c:1164-1170 for a missing file). The port treated an unreadable file
//!    as an empty definition and silently built a new one.
//!
//! 3. `checkout_file()`'s skip-worktree diagnostic carries the flag that gets
//!    past it:
//!
//! ```c
//!     else if (is_skipped)
//!             fprintf(stderr, "has skip-worktree enabled; "
//!                             "use '--ignore-skip-worktree-bits' to checkout");
//! ```
//!
//!    (builtin/checkout-index.c:127-129.) The port stopped at `has
//!    skip-worktree enabled`.
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
    /// A committed worktree with a root file and two directories, which is
    /// enough for cone-mode `set` to have something to include and exclude.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-sc-order-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("folder1")).unwrap();
        std::fs::create_dir_all(work.join("folder2")).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        std::fs::write(f.work.join("folder1/x"), "x\n").unwrap();
        std::fs::write(f.work.join("folder2/y"), "y\n").unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "initial"]);
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
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1112911993 +0000")
            .env("GIT_COMMITTER_DATE", "1112911993 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    /// (stdout, stderr, exit code).
    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.work.join(rel)).unwrap_or_default()
    }
}

/// A cone-mode `set` rejected by `sanitize_paths()` has already recorded the
/// mode, so the worktree counts as sparse afterwards even though no
/// `info/sparse-checkout` exists.
#[test]
fn sparse_checkout_set_records_the_mode_before_it_rejects_a_pattern() {
    let f = Fixture::new("leadingslash");

    let (_, err, code) = f.run(&["sparse-checkout", "set", "/folder1"]);
    assert_eq!(code, 128, "the bad pattern was accepted: {err}");
    assert_eq!(
        err, "fatal: specify directories rather than patterns (no leading slash)\n",
        "wrong diagnostic"
    );

    // `set_config()` -> `init_worktree_config()` flips the extension on in the
    // common config and writes the two keys into the per-worktree config.
    let common = f.read(".git/config");
    assert!(
        common.contains("worktreeConfig = true"),
        "extensions.worktreeConfig was not turned on: {common}"
    );
    let wt = f.read(".git/config.worktree");
    assert!(
        wt.contains("sparseCheckout = true"),
        "core.sparseCheckout was not recorded: {wt}"
    );
    assert!(
        wt.contains("sparseCheckoutCone = true"),
        "core.sparseCheckoutCone was not recorded: {wt}"
    );
    assert!(
        !f.work.join(".git/info/sparse-checkout").exists(),
        "a rejected definition was written to info/sparse-checkout"
    );

    // Config on, pattern file missing: `list` warns and succeeds instead of
    // dying on the `!cfg->apply_sparse_checkout` guard.
    let (out, err, code) = f.run(&["sparse-checkout", "list"]);
    assert_eq!(code, 0, "list died on a config-only sparse worktree: {err}");
    assert_eq!(out, "", "list printed patterns from nowhere");
    assert_eq!(
        err, "warning: this worktree is not sparse (sparse-checkout file may not exist)\n",
        "wrong list diagnostic"
    );
}

/// The state the first test leaves behind — config on, pattern file absent — is
/// exactly what `add` refuses to work from, in both dialects.
#[test]
fn sparse_checkout_add_refuses_a_missing_pattern_file() {
    for (tag, init) in [("cone", "--cone"), ("nocone", "--no-cone")] {
        let f = Fixture::new(tag);
        f.git(&["sparse-checkout", "init", init]);
        std::fs::remove_file(f.work.join(".git/info/sparse-checkout")).unwrap();

        let arg = if init == "--cone" { "folder1" } else { "/folder1/" };
        let (_, err, code) = f.run(&["sparse-checkout", "add", arg]);
        assert_eq!(code, 128, "add invented a definition ({tag}): {err}");
        assert_eq!(
            err, "fatal: unable to load existing sparse-checkout patterns\n",
            "wrong add diagnostic ({tag})"
        );
        assert!(
            !f.work.join(".git/info/sparse-checkout").exists(),
            "the refused add still wrote a pattern file ({tag})"
        );
        // The refusal is total: no worktree was repopulated either.
        assert!(
            !f.work.join("folder1/x").exists(),
            "the refused add still checked a path out ({tag})"
        );
    }
}

/// An *empty* pattern file is not a read failure — `add_patterns()` returns 0
/// for a zero-length file (dir.c:1178-1186) — so `add` proceeds from nothing.
#[test]
fn sparse_checkout_add_accepts_an_empty_pattern_file() {
    let f = Fixture::new("emptyfile");
    f.git(&["sparse-checkout", "init", "--cone"]);
    std::fs::write(f.work.join(".git/info/sparse-checkout"), "").unwrap();

    let (_, err, code) = f.run(&["sparse-checkout", "add", "folder1"]);
    assert_eq!(code, 0, "add refused an empty pattern file: {err}");
    assert_eq!(
        f.read(".git/info/sparse-checkout"),
        "/*\n!/*/\n/folder1/\n",
        "wrong cone definition after adding to an empty file"
    );
}

/// `--sparse-index` is recorded on the same pass, before the refusal.
#[test]
fn sparse_checkout_set_records_index_sparse_before_rejecting() {
    let f = Fixture::new("sparseindex");

    let (_, err, code) = f.run(&["sparse-checkout", "set", "--sparse-index", "/folder1"]);
    assert_eq!(code, 128, "the bad pattern was accepted: {err}");

    let wt = f.read(".git/config.worktree");
    assert!(
        wt.contains("sparse = true"),
        "index.sparse was not recorded: {wt}"
    );
}

/// `checkout-index` names the escape hatch in its skip-worktree refusal.
#[test]
fn checkout_index_skip_worktree_diagnostic_names_the_flag() {
    let f = Fixture::new("skipwt");
    f.git(&["update-index", "--skip-worktree", "a"]);
    std::fs::remove_file(f.work.join("a")).unwrap();

    let (_, err, code) = f.run(&["checkout-index", "a"]);
    assert_eq!(code, 1, "the skipped path was checked out: {err}");
    assert_eq!(
        err,
        "git checkout-index: a has skip-worktree enabled; \
         use '--ignore-skip-worktree-bits' to checkout\n",
        "wrong skip-worktree diagnostic"
    );
    assert!(
        !f.work.join("a").exists(),
        "the refusal still wrote the file"
    );

    // The flag it names does the job, and says nothing while doing it.
    let (_, err, code) = f.run(&["checkout-index", "--ignore-skip-worktree-bits", "a"]);
    assert_eq!(code, 0, "--ignore-skip-worktree-bits failed: {err}");
    assert_eq!(err, "", "the successful checkout was noisy");
    assert_eq!(f.read("a"), "a\n");
}
