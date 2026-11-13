//! Three `diff_options` corners, each against stock git.
//!
//! * `match_filter()` (diff.c:7272-7281) answers `B` for a modification `-B`
//!   gave a score and `M` only for one it did not, and `*` keeps or drops the
//!   whole queue (diff.c:7292-7308). `diff-index` and the `diff-pairs` renderer
//!   `diff-tree -B` goes through kept their own letter lists that knew neither,
//!   so `--diff-filter=B` printed nothing and a lone `*` kept everything.
//!   `diff-pairs` also refused no unknown letter, where `diff_opt_diff_filter()`
//!   says `unknown change class` at exit 129.
//! * `-X[<param>...]` is `--dirstat` (`OPT_CALLBACK_F('X', "dirstat", ...,
//!   PARSE_OPT_OPTARG)`, diff.c:6052-6055); `git diff` did not know it.
//! * `--stat=<width>[,<name-width>[,<count>]]` `strtoul()`s every field a comma
//!   introduces (diff.c:5391-5400), so an empty width is `0` — 80 columns — not
//!   the terminal width a bare `--stat` asks for. `diff-files` knew neither `-X` nor
//!   `strtoul()`: it printed its usage for a value git refuses with `invalid --stat
//!   value`.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> Outcome {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env("HOME", home)
        .env("COLUMNS", "100")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn break_filter_dirstat_short_and_empty_stat_width() {
    let Some(stock) = stock_git::stock_git() else { return };
    let root = std::env::temp_dir().join(format!("zvcs-break-filter-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = Repo(root);
    let work = repo.0.join("w");
    std::fs::create_dir_all(work.join("dir")).unwrap();
    let git = |args: &[&str]| {
        let o = run(stock, &work, &repo.0, args);
        assert_eq!(o.2, Some(0), "{args:?}: {}", o.1);
    };
    let lines = |f: &dyn Fn(usize) -> String| (1..=40).map(|i| f(i) + "\n").collect::<String>();
    git(&["-c", "init.defaultBranch=main", "init", "-q"]);
    std::fs::write(work.join("a.txt"), lines(&|i| format!("line {i} of alpha"))).unwrap();
    std::fs::write(work.join("dir/a_rather_long_file_name_for_the_stat_width.txt"), lines(&|i| format!("c {i}"))).unwrap();
    git(&["add", "."]);
    git(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
    git(&["tag", "c1"]);
    // A complete rewrite, which `-B` breaks, and a small edit, which it does not.
    std::fs::write(work.join("a.txt"), lines(&|i| format!("rewritten {i} zzz"))).unwrap();
    std::fs::write(work.join("dir/a_rather_long_file_name_for_the_stat_width.txt"), lines(&|i| format!("C {i}"))).unwrap();
    git(&["-c", "maintenance.auto=false", "commit", "-q", "-a", "-m", "two"]);
    std::fs::write(work.join("dir/a_rather_long_file_name_for_the_stat_width.txt"), "x\n").unwrap();

    for args in [
        &["diff-tree", "-r", "--name-status", "-B", "--diff-filter=B", "c1", "HEAD"][..],
        &["diff-tree", "-r", "--raw", "-B", "--diff-filter=b", "c1", "HEAD"],
        &["diff-tree", "-r", "--name-status", "-B", "--diff-filter=M*", "c1", "HEAD"],
        &["diff-index", "--name-status", "-B", "--diff-filter=B", "c1"],
        &["diff-index", "--name-status", "--diff-filter=*", "c1"],
        &["diff-index", "--name-status", "-B", "--diff-filter=B*", "c1"],
        &["diff-pairs", "-z", "--diff-filter=Q"],
        &["diff", "-X5", "c1", "HEAD"],
        &["diff", "-X", "c1", "HEAD"],
        &["diff", "-Xfiles,cumulative", "c1", "HEAD"],
        &["diff", "-Xbogus", "c1", "HEAD"],
        &["diff", "--stat=,5", "c1", "HEAD"],
        &["diff", "--stat=60,", "c1", "HEAD"],
        &["diff-files", "-X"],
        &["diff-files", "-X10,files"],
        &["diff-files", "--stat=,5"],
        &["diff-files", "--stat=x"],
        &["diff-files", "--stat-width=x"],
    ] {
        let want = run(stock, &work, &repo.0, args);
        assert_eq!(run(BIN, &work, &repo.0, args), want, "{args:?}");
    }
}
