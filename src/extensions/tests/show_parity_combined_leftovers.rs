//! `show`, `log`, `diff-tree` and `diff` against stock git on the combined-diff
//! leftovers: `log --combined-all-paths` (diff-merges.c:145-146, :184-185), `show
//! -l<n>` (diff.c:6167, :5317-5318), `show --find-object` and the multitree scan's
//! `combined_objfind()` (diff.c:5501-5521, combine-diff.c:1448-1483, :1559-1564),
//! a pathspec limiting the queue before `diffcore_rename()` pairs it
//! (tree-diff.c:320-337), `-G` passing over a binary pair unless `--text` is given
//! (diffcore-pickaxe.c:162-167), and `--diff-filter`'s all-or-none `*`
//! (diff.c:7283-7321).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Repo(PathBuf);

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> (Vec<u8>, Vec<u8>, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (out.stdout, out.stderr, out.status.code().unwrap_or(-1))
}

fn stdout_of(bin: &str, dir: &Path, args: &[&str]) -> String {
    let (out, err, code) = run(bin, dir, args);
    assert_eq!(code, 0, "{args:?}: {}", String::from_utf8_lossy(&err));
    String::from_utf8(out).unwrap().trim_end().to_owned()
}

fn write(dir: &Path, name: &str, body: &[u8]) {
    std::fs::write(dir.join(name), body).unwrap();
}

fn lines(n: u32, edit: &[(u32, &str)]) -> Vec<u8> {
    (1..=n)
        .map(|i| edit.iter().find(|(at, _)| *at == i).map_or(i.to_string(), |(_, s)| s.to_string()) + "\n")
        .collect::<String>()
        .into_bytes()
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn fresh(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-combined-leftovers-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `base`, then `side` and `third` editing `keep`, `both`, `q3` and the binary `bin`,
/// and `main` renaming `ren` to `moved` (with an edit), making `mode` executable and
/// editing `bin`. Two merges are written with `commit-tree` over hand-resolved trees:
/// `oct` (main, side, third), which deletes `del`, adds `added` and edits `moved`
/// again, and `two` (main, side), which renames `ren` to `r2` instead.
fn repo(stock: &str, name: &str) -> Repo {
    let dir = fresh(name);
    let git = |args: &[&str]| stdout_of(stock, &dir, args);
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "maintenance.auto", "false"]);
    write(&dir, "keep", &lines(20, &[]));
    write(&dir, "ren", &lines(30, &[]));
    write(&dir, "del", b"d\n");
    write(&dir, "mode", b"m\n");
    write(&dir, "both", b"x\n");
    write(&dir, "q3", b"q\n");
    write(&dir, "bin", b"a\0b\0c\n");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["checkout", "-q", "-b", "side"]);
    write(&dir, "keep", &lines(20, &[(5, "five")]));
    write(&dir, "both", b"side\n");
    write(&dir, "q3", b"sq\n");
    write(&dir, "bin", b"a\0b side\0\n");
    git(&["commit", "-q", "-a", "-m", "side"]);
    git(&["checkout", "-q", "-b", "third", "main"]);
    write(&dir, "keep", &lines(20, &[(15, "fifteen")]));
    write(&dir, "both", b"third\n");
    write(&dir, "q3", b"t3\n");
    write(&dir, "bin", b"a\0third\0\n");
    git(&["commit", "-q", "-a", "-m", "third"]);
    git(&["checkout", "-q", "main"]);
    git(&["mv", "ren", "moved"]);
    write(&dir, "moved", &lines(30, &[(1, "one")]));
    write(&dir, "both", b"mainb\n");
    write(&dir, "q3", b"mq\n");
    write(&dir, "bin", b"a\0main b\0\n");
    make_executable(&dir.join("mode"));
    git(&["add", "-A", "."]);
    git(&["commit", "-q", "-m", "main"]);

    write(&dir, "keep", &lines(20, &[(5, "x5"), (10, "x10"), (15, "x15")]));
    write(&dir, "both", b"merged\n");
    write(&dir, "q3", b"m3\n");
    write(&dir, "moved", &lines(30, &[(1, "one"), (30, "thirty")]));
    write(&dir, "added", b"new\n");
    write(&dir, "bin", b"a\0merged b\0\n");
    std::fs::remove_file(dir.join("del")).unwrap();
    git(&["add", "-A", "."]);
    let tree = git(&["write-tree"]);
    let oct = git(&["commit-tree", &tree, "-p", "main", "-p", "side", "-p", "third", "-m", "oct"]);
    git(&["tag", "oct", &oct]);

    git(&["checkout", "-q", "-f", "-B", "work", "main~1"]);
    write(&dir, "keep", &lines(20, &[(5, "x5"), (10, "x10")]));
    write(&dir, "both", b"merged2\n");
    write(&dir, "q3", b"s3\n");
    write(&dir, "added", b"new\n");
    write(&dir, "bin", b"a\0two b\0\n");
    std::fs::remove_file(dir.join("del")).unwrap();
    git(&["mv", "ren", "r2"]);
    let mut r2 = lines(30, &[]);
    r2.extend_from_slice(b"z\n");
    write(&dir, "r2", &r2);
    make_executable(&dir.join("mode"));
    git(&["add", "-A", "."]);
    let tree = git(&["write-tree"]);
    let two = git(&["commit-tree", &tree, "-p", "main", "-p", "side", "-m", "two"]);
    git(&["tag", "two", &two]);
    git(&["checkout", "-q", "-f", "main"]);
    Repo(dir)
}

fn same(stock: &str, r: &Repo, cases: &[Vec<String>]) {
    for args in cases {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let ours = run(BIN, &r.0, &args);
        let theirs = run(stock, &r.0, &args);
        assert_eq!(
            (String::from_utf8_lossy(&ours.0), String::from_utf8_lossy(&ours.1), ours.2),
            (String::from_utf8_lossy(&theirs.0), String::from_utf8_lossy(&theirs.1), theirs.2),
            "{args:?}"
        );
    }
}

fn cases(list: &[&str]) -> Vec<Vec<String>> {
    list.iter().map(|line| line.split(' ').map(str::to_owned).collect()).collect()
}

#[test]
fn log_takes_combined_all_paths() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock, "allpaths");
    same(
        stock,
        &r,
        &cases(&[
            "log --combined-all-paths -c --raw --all",
            "log --cc --combined-all-paths --name-status --all",
            "log --cc --combined-all-paths -z --name-only --all",
            "log --combined-all-paths -c -p -1 two",
            "log --combined-all-paths --diff-merges=c --raw -1 two",
            "log --combined-all-paths --raw --all",
            "log --combined-all-paths -c -m --raw -1 two",
            "log -c --combined-all-paths --no-diff-merges --all",
            "log -c --no-diff-merges -1 main",
        ]),
    );
}

#[test]
fn find_object_reaches_show_and_the_multitree_scan() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock, "objfind");
    let added = stdout_of(stock, &r.0, &["rev-parse", "two:added"]);
    let both = stdout_of(stock, &r.0, &["rev-parse", "main:both"]);
    let keep = stdout_of(stock, &r.0, &["rev-parse", "side:keep"]);
    let mut list = Vec::new();
    for (id, rev) in [(&added, "two"), (&both, "two"), (&both, "oct"), (&keep, "oct")] {
        let opt = format!("--find-object={id}");
        for fmt in ["--raw", "--name-status", "-p", "--stat"] {
            for mode in ["-c", "--cc"] {
                list.push(vec!["show".into(), mode.into(), opt.clone(), fmt.into(), rev.into()]);
                list.push(vec!["show".into(), "--no-renames".into(), mode.into(), opt.clone(), fmt.into(), rev.into()]);
                list.push(vec!["diff-tree".into(), mode.into(), opt.clone(), fmt.into(), rev.into()]);
            }
        }
    }
    list.extend(cases(&[
        &format!("show --find-object={keep} --raw side"),
        &format!("show --find-object {keep} --stat side"),
        &format!("show --find-object={keep} -Sx side"),
        &format!("show --find-object={keep} --pickaxe-all side"),
        "show --find-object=nosuch side",
        "show --find-object",
    ]));
    same(stock, &r, &list);
}

#[test]
fn pathspec_limits_rename_sources() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock, "pathspec");
    same(
        stock,
        &r,
        &cases(&[
            "show -M --raw main -- moved",
            "show -M --stat main -- moved",
            "show -C --name-status main -- moved",
            "show -C -C --stat main -- moved",
            "show -M --raw main -- moved ren",
            "show -M -p main -- moved",
        ]),
    );
}

#[test]
fn grep_passes_over_binary_without_text() {
    let Some(stock) = stock_git::stock_git_at_least((2, 32, 0)) else { return };
    let r = repo(stock, "grepbinary");
    same(
        stock,
        &r,
        &cases(&[
            "show --stat -a -Gb side",
            "show --stat -Gb side",
            "show -Gb -a --raw side",
            "show --cc -a --raw -Gb two",
            "show --cc --stat -Gb two",
            "log -1 --cc --stat -Gb oct",
            "log -1 --cc --stat -a -Gb oct",
            "log -c -Gb --raw --all",
            "log -a -Gb --raw --all",
            "diff-tree -c -Gb --raw oct",
            "diff-tree -c -a -Gb --raw oct",
        ]),
    );
}

#[test]
fn diff_filter_star_is_all_or_none() {
    let Some(stock) = stock_git::stock_git() else { return };
    let r = repo(stock, "filterstar");
    same(
        stock,
        &r,
        &cases(&[
            "show --diff-filter=A* --raw two",
            "show --diff-filter=X* --raw two",
            "show --diff-filter=* --raw two",
            "show --diff-filter=*A --name-status main",
            "show --diff-filter=*d --raw two",
            "show --cc --diff-filter=X* --raw oct",
            "log --diff-filter=*R --name-status --all",
            "log --cc --diff-filter=*A --raw --all",
            "diff-tree -c --diff-filter=*D --raw two",
            "diff-tree -r --diff-filter=*R -M main^ main",
            "diff --diff-filter=*R -M --name-status main^ main",
            "diff --diff-filter=*X -M --name-status main^ main",
        ]),
    );
}

/// Three inexactly renamed files, so `-l1` leaves the renames undetected and warns.
#[test]
fn show_takes_rename_limit() {
    let Some(stock) = stock_git::stock_git() else { return };
    let dir = fresh("limit");
    let git = |args: &[&str]| stdout_of(stock, &dir, args);
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "maintenance.auto", "false"]);
    for f in ["a", "b", "c"] {
        let body: String = (1..=20).map(|i| format!("{f} line {i}\n")).collect();
        write(&dir, f, body.as_bytes());
    }
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["checkout", "-q", "-b", "side"]);
    write(&dir, "sidefile", b"s\n");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "side"]);
    git(&["checkout", "-q", "main"]);
    for f in ["a", "b", "c"] {
        let moved = format!("{f}.moved");
        git(&["mv", f, &moved]);
        let mut body = std::fs::read(dir.join(&moved)).unwrap();
        body.extend_from_slice(b"extra\n");
        write(&dir, &moved, &body);
    }
    git(&["add", "-A", "."]);
    git(&["commit", "-q", "-m", "renames"]);
    git(&["merge", "-q", "--no-edit", "side"]);
    let r = Repo(dir);
    same(
        stock,
        &r,
        &cases(&[
            "show -l1 HEAD~1",
            "show -l1 --stat HEAD~1",
            "show -l 1 --raw HEAD~1",
            "show -l0 --raw HEAD~1",
            "show -l1 -m --raw HEAD",
            "show -l1 --cc --stat HEAD",
            "show -l1 -C -C --raw HEAD~1",
            "show -l1x HEAD~1",
            "show -l",
        ]),
    );
}
