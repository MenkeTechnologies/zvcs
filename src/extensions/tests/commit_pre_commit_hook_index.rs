//! What `git commit` does with the index around the `pre-commit` hook.
//!
//! The hook can write the index — that is the whole point of the auto-formatter
//! pattern, `rustfmt`/`prettier`/`black`/`gofmt` followed by `git add` — and git
//! has a precise rule for whose writes win:
//!
//! ```c
//! if (!no_verify && invoked_hook) {
//!         /*
//!          * Re-read the index as the pre-commit-commit hook was invoked
//!          * and could have updated it. We must do this before we invoke
//!          * the editor and after we invoke run_status above.
//!          */
//!         discard_index(the_repository->index);
//! }
//! read_index_from(the_repository->index, index_file, repo_get_git_dir(the_repository));
//! ```
//!
//! (builtin/commit.c:1101-1109.) Three details carry the whole behaviour:
//!
//! * the `read_index_from()` is unconditional but returns immediately while the
//!   index is still loaded (read-cache.c:2371-2372, and `do_read_index` again at
//!   `:2225-2226`), so the `discard_index()` above it is the entire mechanism;
//! * it keys on `invoked_hook`, which `run_hooks_opt()` sets only when a hook was
//!   actually executed (hook.c:659-660, cleared at `:823-824`);
//! * it re-reads `index_file` — what `prepare_index()` chose and what the hook was
//!   pointed at through `GIT_INDEX_FILE` (commit.c:1994) — which is the real index
//!   for a plain commit and a *temporary* one for `--only`.
//!
//! Without the re-read the commit records the index as it was read *before* the
//! hook and the hook's staging is silently dropped, with a successful exit and no
//! warning. These tests pin the rule from both sides: what must now survive, and
//! what must still not leak in.
//!
//! Every stock-git comparison degrades to a skip when no stock git is on the
//! machine, so the file is safe in a headless CI that only has the binary under
//! test. Nothing here needs a daemon.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

// ---------------------------------------------------------------------------
// process plumbing
// ---------------------------------------------------------------------------

/// Run `bin` in `cwd` with an isolated, deterministic environment so no ambient
/// config or identity can reach the run.
fn run_with(bin: &str, cwd: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "zvcs-test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "zvcs-test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00+0000")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00+0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap_or_else(|e| panic!("{bin} {args:?}: {e}"))
}

/// [`run_with`] asserting success and returning trimmed stdout.
fn ok_with(bin: &str, cwd: &Path, args: &[&str]) -> String {
    let out = run_with(bin, cwd, args);
    assert!(
        out.status.success(),
        "`{bin} {args:?}` failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// A stock git that is definitely *not* this binary, or `None` to skip.
///
/// `zjobs` is a zvcs-only verb: stock git fails on it, this binary succeeds. The
/// probe runs with an **empty `PATH`** because git resolves an unknown verb by
/// looking for `git-<verb>` on `PATH` (`execv_dashed_external()`), and a machine
/// with the shadow binary installed has a `git-zjobs` symlink sitting there — so
/// with the ambient `PATH`, stock git would dispatch into zvcs and the probe
/// would mistake it for the binary under test.
fn stock_git() -> Option<String> {
    fn on_path(name: &str) -> Option<String> {
        if name.contains('/') {
            return Some(name.to_string());
        }
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(name))
                .find(|c| c.is_file())
                .map(|c| c.to_string_lossy().into_owned())
        })
    }

    for cand in ["/opt/homebrew/bin/git", "/usr/bin/git", "/usr/local/bin/git", "git"] {
        let Some(cand) = on_path(cand) else { continue };
        let Ok(version) = Command::new(&cand).arg("--version").output() else { continue };
        if !version.status.success() || !version.stdout.starts_with(b"git version") {
            continue;
        }
        match Command::new(&cand).arg("zjobs").env("PATH", "").output() {
            Ok(out) if !out.status.success() => return Some(cand),
            _ => continue,
        }
    }
    None
}

/// A fresh, empty directory named after `tag`.
fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "zvcs-precommit-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

// ---------------------------------------------------------------------------
// repository fixture
// ---------------------------------------------------------------------------

fn write(repo: &Path, rel: &str, body: &str) {
    let path = repo.join(rel);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

/// Install `<repo>/.git/hooks/<event>` as a `/bin/sh` script.
///
/// `body` is spliced in after the shebang; `{git}` in it is replaced with the
/// **absolute** path of the binary the hook should call, so the hook never
/// depends on `PATH` — a headless runner has no reason to have either binary on
/// it, and picking one up by accident would silently test the wrong thing.
fn hook(repo: &Path, event: &str, bin: &str, body: &str) {
    let dir = repo.join(".git/hooks");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(event);
    std::fs::write(&path, format!("#!/bin/sh\n{}\n", body.replace("{git}", bin))).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A repository with one commit containing `base.txt`, built by `bin`.
fn repo_with_base(bin: &str, tag: &str) -> PathBuf {
    let dir = tmp(tag);
    ok_with(bin, &dir, &["init", "-q", "-b", "main", "."]);
    write(&dir, "base.txt", "base\n");
    ok_with(bin, &dir, &["add", "base.txt"]);
    ok_with(bin, &dir, &["commit", "-q", "-m", "base"]);
    dir
}

/// The paths `HEAD`'s tree records, sorted.
fn head_paths(bin: &str, repo: &Path) -> Vec<String> {
    ok_with(bin, repo, &["ls-tree", "-r", "--name-only", "HEAD"])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The blob `HEAD` records at `rel`, or `None` when the path is not in the tree.
fn head_blob(bin: &str, repo: &Path, rel: &str) -> Option<String> {
    let out = run_with(bin, repo, &["show", &format!("HEAD:{rel}")]);
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The blob the *index* holds at `rel`, or `None` when the path is unstaged.
fn index_blob(bin: &str, repo: &Path, rel: &str) -> Option<String> {
    let out = run_with(bin, repo, &["show", &format!(":{rel}")]);
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn commit_count(bin: &str, repo: &Path) -> usize {
    ok_with(bin, repo, &["rev-list", "--count", "HEAD"]).parse().unwrap()
}

// ---------------------------------------------------------------------------
// the defect: a hook that stages must have its staging committed
// ---------------------------------------------------------------------------

/// The auto-formatter pattern. The hook writes a file and stages it; git commits
/// it, because `discard_index()` (builtin/commit.c:1107) threw away the index that
/// was read before the hook ran.
///
/// This is the test that fails without the re-read: the commit still *succeeds*,
/// so a port that skips it reports nothing wrong and quietly drops the hook's
/// work.
#[test]
fn pre_commit_hook_staging_reaches_the_commit() {
    let repo = repo_with_base(BIN, "stage-reaches");
    hook(&repo, "pre-commit", BIN, "printf 'formatted\\n' > fmt.txt\nexec {git} add fmt.txt");

    write(&repo, "main.txt", "main\n");
    ok_with(BIN, &repo, &["add", "main.txt"]);
    ok_with(BIN, &repo, &["commit", "-q", "-m", "with hook"]);

    assert_eq!(head_paths(BIN, &repo), ["base.txt", "fmt.txt", "main.txt"]);
    assert_eq!(head_blob(BIN, &repo, "fmt.txt").as_deref(), Some("formatted\n"));
    // And the staging is still staged afterwards: the hook wrote the real index,
    // and a plain commit's `index_file` *is* the real index (commit.c:493).
    assert_eq!(index_blob(BIN, &repo, "fmt.txt").as_deref(), Some("formatted\n"));
}

/// `-a` reaches the same re-read. git points its hook at the index lock it is
/// holding rather than at `.git/index` (commit.c:468), but either way what the
/// hook staged is what the commit records.
#[test]
fn commit_all_picks_up_hook_staging() {
    let repo = repo_with_base(BIN, "all");
    hook(&repo, "pre-commit", BIN, "printf 'formatted\\n' > fmt.txt\nexec {git} add fmt.txt");

    write(&repo, "base.txt", "changed\n");
    ok_with(BIN, &repo, &["commit", "-q", "-a", "-m", "all"]);

    assert_eq!(head_paths(BIN, &repo), ["base.txt", "fmt.txt"]);
    assert_eq!(head_blob(BIN, &repo, "base.txt").as_deref(), Some("changed\n"));
}

/// `--amend` is git's as-is path too, so the replacement commit carries what the
/// hook staged.
#[test]
fn amend_picks_up_hook_staging() {
    let repo = repo_with_base(BIN, "amend");
    write(&repo, "main.txt", "main\n");
    ok_with(BIN, &repo, &["add", "main.txt"]);
    ok_with(BIN, &repo, &["commit", "-q", "-m", "pre"]);

    hook(&repo, "pre-commit", BIN, "printf 'formatted\\n' > fmt.txt\nexec {git} add fmt.txt");
    ok_with(BIN, &repo, &["commit", "-q", "--amend", "-m", "amended"]);

    assert_eq!(commit_count(BIN, &repo), 2);
    assert_eq!(head_paths(BIN, &repo), ["base.txt", "fmt.txt", "main.txt"]);
}

/// A hook that exits non-zero vetoes the commit — `run_commit_hook()` returning
/// non-zero makes `prepare_to_commit()` return 0 (commit.c:780-782), and
/// `cmd_commit` then rolls back and exits 1 (`:1842-1847`). Nothing is recorded,
/// however much the hook staged on its way out.
#[test]
fn failing_hook_aborts_and_commits_nothing() {
    let repo = repo_with_base(BIN, "veto");
    hook(&repo, "pre-commit", BIN, "printf 'formatted\\n' > fmt.txt\n{git} add fmt.txt\nexit 1");

    write(&repo, "main.txt", "main\n");
    ok_with(BIN, &repo, &["add", "main.txt"]);
    let out = run_with(BIN, &repo, &["commit", "-q", "-m", "should abort"]);

    assert!(!out.status.success(), "a vetoing pre-commit hook must fail the commit");
    assert_eq!(commit_count(BIN, &repo), 1);
    assert_eq!(head_paths(BIN, &repo), ["base.txt"]);
}

/// `--no-verify` skips the hook entirely, so there is nothing to re-read for and
/// nothing the hook could have staged.
#[test]
fn no_verify_skips_the_hook_and_its_staging() {
    let repo = repo_with_base(BIN, "no-verify");
    hook(&repo, "pre-commit", BIN, "printf 'formatted\\n' > fmt.txt\nexec {git} add fmt.txt");

    write(&repo, "main.txt", "main\n");
    ok_with(BIN, &repo, &["add", "main.txt"]);
    ok_with(BIN, &repo, &["commit", "-q", "--no-verify", "-m", "unverified"]);

    assert_eq!(head_paths(BIN, &repo), ["base.txt", "main.txt"]);
    assert!(!repo.join("fmt.txt").exists(), "the hook must not have run at all");
}

// ---------------------------------------------------------------------------
// `--only`: the re-read must not widen what a partial commit records
// ---------------------------------------------------------------------------

/// The guarantee `--only` exists for: a path the user staged but did not name
/// stays out of the commit. The re-read must not reach the real index and drag it
/// in — the partial commit's base is HEAD's tree, not the index.
#[test]
fn only_mode_ignores_unnamed_paths_the_user_staged() {
    let repo = repo_with_base(BIN, "only-unnamed");
    hook(&repo, "pre-commit", BIN, "printf 'sneak\\n' > unrelated.txt\nexec {git} add unrelated.txt");

    write(&repo, "wanted.txt", "w\n");
    write(&repo, "other.txt", "x\n");
    ok_with(BIN, &repo, &["add", "wanted.txt", "other.txt"]);
    write(&repo, "wanted.txt", "w2\n");
    write(&repo, "other.txt", "x2\n");
    ok_with(BIN, &repo, &["commit", "-q", "--only", "wanted.txt", "-m", "only"]);

    // `other.txt` was staged before the commit and is still not in the tree.
    assert!(
        head_blob(BIN, &repo, "other.txt").is_none(),
        "--only committed a path the user staged but did not name"
    );
    assert_eq!(head_blob(BIN, &repo, "wanted.txt").as_deref(), Some("w2\n"));
}

/// The false index is built *before* the hook runs (`prepare_index()`,
/// commit.c:541-555, precedes `prepare_to_commit()` at `:1842`), so a hook that
/// rewrites a named file without staging it changes nothing: `--only` records the
/// worktree as it stood when the command started.
#[test]
fn only_mode_builds_its_tree_before_the_hook_runs() {
    let repo = repo_with_base(BIN, "only-prehook");
    write(&repo, "wanted.txt", "w\n");
    ok_with(BIN, &repo, &["add", "wanted.txt"]);
    ok_with(BIN, &repo, &["commit", "-q", "-m", "seed"]);

    hook(&repo, "pre-commit", BIN, "printf 'HOOKED\\n' > wanted.txt");
    write(&repo, "wanted.txt", "w2\n");
    ok_with(BIN, &repo, &["commit", "-q", "--only", "wanted.txt", "-m", "only"]);

    assert_eq!(
        head_blob(BIN, &repo, "wanted.txt").as_deref(),
        Some("w2\n"),
        "--only must record the worktree as of the start of the command"
    );
}

/// Under `--only` the hook is pointed at the temporary index the commit is built
/// from, not at the repository's own (commit.c:554 with the `GIT_INDEX_FILE`
/// export at `:1994`). So what it stages lands in the *commit* and the real index
/// is left alone — and the temporary file is gone afterwards, git's
/// `rollback_lock_file(&false_lock)` (commit.c:243-244).
#[test]
fn only_mode_hook_stages_into_the_temporary_index() {
    let repo = repo_with_base(BIN, "only-temp");
    hook(&repo, "pre-commit", BIN, "printf 'sneak\\n' > unrelated.txt\nexec {git} add unrelated.txt");

    write(&repo, "wanted.txt", "w\n");
    // `--only` matches against the index unioned with HEAD, so the path has to be
    // known to git before it can be named.
    ok_with(BIN, &repo, &["add", "wanted.txt"]);
    ok_with(BIN, &repo, &["commit", "-q", "--only", "wanted.txt", "-m", "only"]);

    assert_eq!(head_blob(BIN, &repo, "unrelated.txt").as_deref(), Some("sneak\n"));
    assert!(
        index_blob(BIN, &repo, "unrelated.txt").is_none(),
        "the hook's staging must have gone to the temporary index, not the real one"
    );

    let leftovers: Vec<_> = std::fs::read_dir(repo.join(".git"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.starts_with("next-index-"))
        .collect();
    assert!(leftovers.is_empty(), "temporary index left behind: {leftovers:?}");
}

/// The same, for the abort path: a vetoing hook must not leave the temporary
/// index lying in the git directory either.
#[test]
fn only_mode_cleans_up_after_a_vetoing_hook() {
    let repo = repo_with_base(BIN, "only-veto");
    hook(&repo, "pre-commit", BIN, "exit 1");

    write(&repo, "wanted.txt", "w\n");
    ok_with(BIN, &repo, &["add", "wanted.txt"]);
    let out = run_with(BIN, &repo, &["commit", "-q", "--only", "wanted.txt", "-m", "only"]);
    assert!(!out.status.success());

    let leftovers: Vec<_> = std::fs::read_dir(repo.join(".git"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.starts_with("next-index-"))
        .collect();
    assert!(leftovers.is_empty(), "temporary index left behind: {leftovers:?}");
}

// ---------------------------------------------------------------------------
// only `pre-commit` gets the re-read
// ---------------------------------------------------------------------------

/// `commit-msg` runs after `cache_tree_update()` has already built the tree
/// (commit.c:1111 then `:1133`) and there is no second `discard_index()` between
/// them — `discard_index` appears on the commit path exactly once, at `:1107`. So
/// a `commit-msg` hook that stages leaves the path staged and out of the commit.
#[test]
fn commit_msg_hook_staging_stays_out_of_the_commit() {
    let repo = repo_with_base(BIN, "commit-msg");
    hook(&repo, "commit-msg", BIN, "printf 'late\\n' > late.txt\n{git} add late.txt\nexit 0");

    write(&repo, "main.txt", "main\n");
    ok_with(BIN, &repo, &["add", "main.txt"]);
    ok_with(BIN, &repo, &["commit", "-q", "-m", "msg hook"]);

    assert_eq!(head_paths(BIN, &repo), ["base.txt", "main.txt"]);
    assert_eq!(index_blob(BIN, &repo, "late.txt").as_deref(), Some("late\n"));
}

// ---------------------------------------------------------------------------
// the hook has to be runnable from anywhere in the work tree
// ---------------------------------------------------------------------------

/// git `chdir`s to the top of the work tree during setup, so its own relative
/// `.git/hooks/pre-commit` still resolves once the hook is spawned there. zvcs
/// stays where it was invoked, and from a subdirectory the same file is spelled
/// `../.git/hooks/pre-commit` — handing *that* to a child whose working directory
/// has been moved to the work tree root pointed the exec one level above the
/// repository, and `git commit` from any subdirectory of a repository with any
/// hook installed died with `No such file or directory`.
#[test]
fn hook_runs_when_the_commit_is_made_from_a_subdirectory() {
    let repo = repo_with_base(BIN, "subdir");
    hook(
        &repo,
        "pre-commit",
        BIN,
        "printf '%s\\n' \"$(pwd)\" > hook-cwd\nprintf 'formatted\\n' > fmt.txt\nexec {git} add fmt.txt",
    );

    write(&repo, "deep/main.txt", "main\n");
    ok_with(BIN, &repo, &["add", "deep/main.txt"]);
    ok_with(BIN, &repo.join("deep"), &["commit", "-q", "-m", "from deep"]);

    assert_eq!(head_paths(BIN, &repo), ["base.txt", "deep/main.txt", "fmt.txt"]);
    // git runs hooks at the top of the work tree, whatever directory the command
    // was issued from.
    let cwd = std::fs::read_to_string(repo.join("hook-cwd")).unwrap();
    assert_eq!(
        Path::new(cwd.trim()).canonicalize().unwrap(),
        repo,
        "the hook must run at the top of the work tree"
    );
}

// ---------------------------------------------------------------------------
// agreement with stock git
// ---------------------------------------------------------------------------

/// The same five scenarios run against stock git and against this binary, in
/// their own repositories, compared on what ends up in `HEAD` and in the index.
///
/// This is the check that would have caught the defect as a *difference* rather
/// than as an opinion: every one of these commits succeeds under both binaries,
/// and only the recorded content tells them apart.
#[test]
fn matches_stock_git_across_the_commit_modes() {
    let Some(stock) = stock_git() else {
        eprintln!("no stock git available; skipping interop comparison");
        return;
    };

    /// One scenario, as a closure over the binary that runs it, returning a
    /// comparable summary of the resulting repository.
    fn scenario(bin: &str, tag: &str, case: &str) -> String {
        let repo = repo_with_base(bin, tag);
        match case {
            "plain" => {
                hook(&repo, "pre-commit", bin, "printf 'formatted\\n' > fmt.txt\nexec {git} add fmt.txt");
                write(&repo, "main.txt", "main\n");
                ok_with(bin, &repo, &["add", "main.txt"]);
                let _ = run_with(bin, &repo, &["commit", "-q", "-m", "m"]);
            }
            "all" => {
                hook(&repo, "pre-commit", bin, "printf 'formatted\\n' > fmt.txt\nexec {git} add fmt.txt");
                write(&repo, "base.txt", "changed\n");
                let _ = run_with(bin, &repo, &["commit", "-q", "-a", "-m", "m"]);
            }
            "amend" => {
                write(&repo, "main.txt", "main\n");
                ok_with(bin, &repo, &["add", "main.txt"]);
                ok_with(bin, &repo, &["commit", "-q", "-m", "pre"]);
                hook(&repo, "pre-commit", bin, "printf 'formatted\\n' > fmt.txt\nexec {git} add fmt.txt");
                let _ = run_with(bin, &repo, &["commit", "-q", "--amend", "-m", "m"]);
            }
            "veto" => {
                hook(&repo, "pre-commit", bin, "printf 'formatted\\n' > fmt.txt\n{git} add fmt.txt\nexit 1");
                write(&repo, "main.txt", "main\n");
                ok_with(bin, &repo, &["add", "main.txt"]);
                let _ = run_with(bin, &repo, &["commit", "-q", "-m", "m"]);
            }
            "only" => {
                hook(&repo, "pre-commit", bin, "printf 'sneak\\n' > unrelated.txt\nexec {git} add unrelated.txt");
                write(&repo, "wanted.txt", "w\n");
                write(&repo, "other.txt", "x\n");
                ok_with(bin, &repo, &["add", "wanted.txt", "other.txt"]);
                write(&repo, "wanted.txt", "w2\n");
                let _ = run_with(bin, &repo, &["commit", "-q", "--only", "wanted.txt", "-m", "m"]);
            }
            other => panic!("unknown scenario {other}"),
        }
        let tree = head_paths(bin, &repo).join(",");
        let staged = ok_with(bin, &repo, &["diff", "--cached", "--name-only"])
            .lines()
            .collect::<Vec<_>>()
            .join(",");
        format!("commits={} tree=[{tree}] staged=[{staged}]", commit_count(bin, &repo))
    }

    for case in ["plain", "all", "amend", "veto", "only"] {
        let ours = scenario(BIN, &format!("interop-ours-{case}"), case);
        let theirs = scenario(&stock, &format!("interop-stock-{case}"), case);
        assert_eq!(ours, theirs, "`{case}` differs from stock git");
    }
}
