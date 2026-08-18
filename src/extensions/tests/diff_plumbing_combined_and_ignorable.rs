//! Plumbing diff capabilities that were refused or silently downgraded while the
//! same binary already rendered them somewhere else.
//!
//! Regressions, all measured against stock git 2.55.0:
//!
//! * `git diff-tree --cc <merge>` died with `combined patch output (--cc) is not
//!   ported` even though `git show`, `git log --cc` and `git diff <a> <b> <c>` all
//!   print the combined patch from the shared engine.
//! * `git diff-tree -c -p <merge>` printed the combined *raw* listing at exit 0 —
//!   the wrong format, with no diagnostic.
//! * `git diff-tree -c --stat`/`--numstat` printed the combined raw listing too.
//!   `diff_tree_combined()` answers the stat family with a plain two-way diff
//!   against the *first parent*, and emits it even when no path reaches the
//!   combined diff at all.
//! * `git diff-tree --submodule` and `--ignore-submodules` were refused outright.
//! * `git diff-files -p --ignore-blank-lines` printed the full hunk for a file
//!   whose only change is blank lines; stock prints nothing for it.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// PATH with any zvcs shadow dir removed, so a nested `git` in setup resolves to
/// the real system git rather than recursing into the binary under test.
fn real_git_path() -> String {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|p| !p.contains(".zvcs"))
        .collect::<Vec<_>>()
        .join(":")
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(["-c", "user.email=t@e.x", "-c", "user.name=t", "-c", "protocol.file.allow=always"])
        .args(args)
        .env("PATH", real_git_path())
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

/// Run and require success, returning stdout with the trailing newline kept.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// The same, trimmed — for the plumbing commands whose whole answer is one id.
fn git_line(dir: &Path, args: &[&str]) -> String {
    git(dir, args).trim_end().to_owned()
}

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).unwrap();
}

fn temp_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-dpci-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

fn init(tag: &str) -> std::path::PathBuf {
    let root = temp_root(tag);
    git(&root, &["init", "-q", "-b", "main", "."]);
    root
}

/// Stage the working tree and commit it on top of `parents`, moving `HEAD` there.
///
/// Built from `write-tree`/`commit-tree` rather than `git merge` so the fixture
/// does not depend on the merge machinery to produce a two-parent commit.
fn commit(dir: &Path, parents: &[&str]) -> String {
    git(dir, &["add", "-A"]);
    let tree = git_line(dir, &["write-tree"]);
    let mut args: Vec<String> = vec!["commit-tree".into(), tree, "-m".into(), "c".into()];
    for p in parents {
        args.push("-p".into());
        args.push((*p).to_owned());
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let id = git_line(dir, &refs);
    git(dir, &["update-ref", "HEAD", &id]);
    id
}

/// A merge whose single conflicted path differs from both parents, so it is the
/// one path a combined diff reports.
///
/// `f` is `A-SIDE` in parent 1, `B-SIDE` in parent 2 and `RESOLVED` in the merge.
/// `g` additionally moves eight lines away from parent 2 but only one away from
/// parent 1, which is what tells a first-parent stat from a second-parent one.
fn merge_fixture(tag: &str) -> (std::path::PathBuf, String) {
    let dir = init(tag);
    write(&dir, "f", "BASE\n");
    write(&dir, "g", "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n");
    let base = commit(&dir, &[]);

    write(&dir, "f", "A-SIDE\n");
    write(&dir, "g", "A1\nA2\nA3\nA4\nA5\nA6\nA7\nA8\n");
    let p1 = commit(&dir, &[&base]);

    git(&dir, &["update-ref", "HEAD", &base]);
    git(&dir, &["reset", "-q", "--hard", &base]);
    write(&dir, "f", "B-SIDE\n");
    write(&dir, "g", "l1\nl2\nl3\nl4\nl5\nl6\nl7\nB8\n");
    let p2 = commit(&dir, &[&base]);

    git(&dir, &["update-ref", "HEAD", &p1]);
    git(&dir, &["reset", "-q", "--hard", &p1]);
    write(&dir, "f", "RESOLVED\n");
    write(&dir, "g", "A1\nA2\nA3\nA4\nA5\nA6\nA7\nZZ\n");
    let merge = commit(&dir, &[&p1, &p2]);
    (dir, merge)
}

/// The same shape with `f` alone, so the combined patch has exactly one file.
fn merge_fixture_one_path(tag: &str) -> (std::path::PathBuf, String) {
    let dir = init(tag);
    write(&dir, "f", "BASE\n");
    let base = commit(&dir, &[]);

    write(&dir, "f", "A-SIDE\n");
    let p1 = commit(&dir, &[&base]);

    git(&dir, &["update-ref", "HEAD", &base]);
    git(&dir, &["reset", "-q", "--hard", &base]);
    write(&dir, "f", "B-SIDE\n");
    let p2 = commit(&dir, &[&base]);

    git(&dir, &["update-ref", "HEAD", &p1]);
    git(&dir, &["reset", "-q", "--hard", &p1]);
    write(&dir, "f", "RESOLVED\n");
    let merge = commit(&dir, &[&p1, &p2]);
    (dir, merge)
}

/// Replace each `index <a>,<b>..<c>` line with a fixed marker after checking its
/// shape, so the expectations below can be exact without hard-coding object ids.
fn mask_index(out: &str) -> String {
    out.lines()
        .map(|l| {
            let Some(rest) = l.strip_prefix("index ") else {
                return l.to_owned();
            };
            let hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
            let (pre, post) = rest.split_once("..").expect("combined index line has `..`");
            let (a, b) = pre.split_once(',').expect("combined index line has two parents");
            assert!(hex(a) && hex(b) && hex(post), "malformed index line: {l}");
            assert_eq!(a.len(), b.len(), "parent abbreviations disagree: {l}");
            "index <IDS>".to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// `--cc` renders the dense combined patch — `diff --cc` header, `@@@` hunk.
#[test]
fn diff_tree_cc_renders_the_combined_patch() {
    let (dir, merge) = merge_fixture_one_path("cc");
    let out = run(&dir, &["diff-tree", "--cc", &merge]);
    assert!(out.status.success(), "--cc exited {}", out.status);
    let got = mask_index(&String::from_utf8(out.stdout).unwrap());
    let want = format!(
        "{merge}\n\
         diff --cc f\n\
         index <IDS>\n\
         --- a/f\n\
         +++ b/f\n\
         @@@ -1,1 -1,1 +1,1 @@@\n\
         - A-SIDE\n\
         \x20-B-SIDE\n\
         ++RESOLVED\n"
    );
    assert_eq!(got, want);
}

/// A bare `-c` with `-p` selects the same engine but the `diff --combined` header,
/// not the combined *raw* listing it used to print at exit 0.
#[test]
fn diff_tree_c_p_renders_the_combined_header() {
    let (dir, merge) = merge_fixture_one_path("cp");
    let out = run(&dir, &["diff-tree", "-c", "-p", &merge]);
    assert!(out.status.success(), "-c -p exited {}", out.status);
    let got = mask_index(&String::from_utf8(out.stdout).unwrap());
    assert!(got.contains("diff --combined f\n"), "no `diff --combined` header in:\n{got}");
    assert!(!got.contains("diff --cc"), "`-c` must not use the dense header:\n{got}");
    assert!(got.contains("@@@ -1,1 -1,1 +1,1 @@@\n"), "no combined hunk in:\n{got}");
    assert!(!got.contains("::100644"), "still the combined raw listing:\n{got}");
}

/// The stat family under `-c`/`--cc` is a two-way diff against the **first**
/// parent. `g` is one line from parent 1 and eight from parent 2, so the counts
/// say which one ran.
#[test]
fn diff_tree_combined_stat_is_against_the_first_parent() {
    let (dir, merge) = merge_fixture("stat");
    for flag in ["-c", "--cc"] {
        // Against parent 2, `g` would be 8 insertions and 8 deletions.
        let got = git(&dir, &["diff-tree", flag, "--stat", &merge]);
        assert_eq!(
            got,
            format!(
                "{merge}\n f | 2 +-\n g | 2 +-\n \
                 2 files changed, 2 insertions(+), 2 deletions(-)\n"
            ),
            "{flag} --stat"
        );

        let got = git(&dir, &["diff-tree", flag, "--shortstat", &merge]);
        assert_eq!(
            got,
            format!("{merge}\n 2 files changed, 2 insertions(+), 2 deletions(-)\n"),
            "{flag} --shortstat"
        );
    }
}

/// `diff_tree_combined()` emits the stat before it looks at the combined path set,
/// so a merge with no path differing from every parent still gets a diffstat where
/// a bare `-c` prints nothing but the commit id.
#[test]
fn diff_tree_combined_stat_survives_an_empty_path_set() {
    let dir = init("cleanstat");
    write(&dir, "a", "a1\n");
    write(&dir, "b", "b1\n");
    let base = commit(&dir, &[]);

    write(&dir, "a", "a2\n");
    let p1 = commit(&dir, &[&base]);

    git(&dir, &["update-ref", "HEAD", &base]);
    git(&dir, &["reset", "-q", "--hard", &base]);
    write(&dir, "b", "b2\n");
    let p2 = commit(&dir, &[&base]);

    // The merge takes `a` from parent 1 and `b` from parent 2, so neither path
    // differs from *both* parents and the combined path set is empty.
    git(&dir, &["update-ref", "HEAD", &p1]);
    git(&dir, &["reset", "-q", "--hard", &p1]);
    write(&dir, "b", "b2\n");
    let merge = commit(&dir, &[&p1, &p2]);

    assert_eq!(git(&dir, &["diff-tree", "-c", &merge]), format!("{merge}\n"), "raw");
    assert_eq!(
        git(&dir, &["diff-tree", "-c", "--shortstat", &merge]),
        format!("{merge}\n 1 file changed, 1 insertion(+), 1 deletion(-)\n"),
        "the first-parent stat still runs"
    );
}

/// `-c --exit-code` reports 0: `diff_tree_combined()` never queues a pair on the
/// caller's `diffopt`, so `DIFF_OPT_HAS_CHANGES` stays clear.
#[test]
fn diff_tree_combined_does_not_set_exit_code() {
    let (dir, merge) = merge_fixture("exit");
    for flag in ["-c", "--cc"] {
        let out = run(&dir, &["diff-tree", flag, "--exit-code", &merge]);
        assert_eq!(out.status.code(), Some(0), "{flag} --exit-code on a differing merge");
        assert!(!out.stdout.is_empty(), "{flag} printed nothing at all");
    }
}

/// `-m`, `-c` and `--cc` are three settings of one knob, so the last one wins.
#[test]
fn diff_tree_merge_selectors_are_last_wins() {
    let (dir, merge) = merge_fixture("lastwins");
    let separate = git(&dir, &["diff-tree", "--cc", "-m", &merge]);
    assert!(separate.contains(":100644 100644 "), "`--cc -m` must be the per-parent form:\n{separate}");
    assert_eq!(separate.matches(&merge).count(), 2, "one commit-id line per parent:\n{separate}");

    let combined = git(&dir, &["diff-tree", "-m", "--cc", &merge]);
    assert!(combined.contains("diff --cc f\n"), "`-m --cc` must be the combined patch:\n{combined}");
}

/// A superproject with `sub` at its second commit, and the ids of both.
fn submodule_fixture(tag: &str) -> (std::path::PathBuf, String, String) {
    let root = temp_root(tag);
    let src = root.join("sub_src");
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main", "."]);
    write(&src, "s", "one\n");
    let s1 = commit(&src, &[]);
    write(&src, "s", "two\n");
    let s2 = commit(&src, &[&s1]);

    let sup = root.join("super");
    std::fs::create_dir_all(&sup).unwrap();
    git(&sup, &["init", "-q", "-b", "main", "."]);
    write(&sup, "keep", "keep\n");
    git(&sup, &["submodule", "add", "-q", src.to_str().unwrap(), "sub"]);
    let first = commit(&sup, &[]);

    // Move the gitlink back to the submodule's first commit.
    git(&sup, &["update-index", "--cacheinfo", &format!("160000,{s1},sub")]);
    let tree = git_line(&sup, &["write-tree"]);
    let second =
        git_line(&sup, &["commit-tree", &tree, "-p", &first, "-m", "bump"]);
    git(&sup, &["update-ref", "HEAD", &second]);
    (sup, first, second)
}

/// `--ignore-submodules[=all]` drops the pairs that only ever name a gitlink; the
/// other three values name worktree states two trees cannot have.
#[test]
fn diff_tree_ignore_submodules_drops_the_gitlink_row() {
    let (dir, a, b) = submodule_fixture("ignsub");
    let plain = git(&dir, &["diff-tree", "-r", &a, &b]);
    assert!(plain.contains(":160000 160000 "), "fixture has no gitlink change:\n{plain}");

    for flag in ["--ignore-submodules", "--ignore-submodules=all"] {
        assert_eq!(git(&dir, &["diff-tree", "-r", flag, &a, &b]), "", "{flag}");
    }
    for flag in ["--ignore-submodules=none", "--ignore-submodules=dirty", "--ignore-submodules=untracked"] {
        assert_eq!(git(&dir, &["diff-tree", "-r", flag, &a, &b]), plain, "{flag}");
    }
}

/// `--submodule` replaces the gitlink pair's patch body with the summary form.
#[test]
fn diff_tree_submodule_renders_the_summary_form() {
    let (dir, a, b) = submodule_fixture("subfmt");
    let got = git(&dir, &["diff-tree", "-r", "-p", "--submodule", &a, &b]);
    assert!(got.starts_with("Submodule sub "), "not the summary form:\n{got}");
    assert!(!got.contains("Subproject commit"), "still the plain gitlink patch:\n{got}");

    // `--submodule=short` is the default rendering, so it must not change it.
    let plain = git(&dir, &["diff-tree", "-r", "-p", &a, &b]);
    assert_eq!(git(&dir, &["diff-tree", "-r", "-p", "--submodule=short", &a, &b]), plain);

    // The raw listing is untouched by either.
    let raw = git(&dir, &["diff-tree", "-r", &a, &b]);
    assert_eq!(git(&dir, &["diff-tree", "-r", "--submodule", &a, &b]), raw);
}

/// Two worktree modifications: `only_blank` gains nothing but blank lines,
/// `mixed` gains a blank line three lines above a real edit.
fn blank_fixture(tag: &str) -> std::path::PathBuf {
    let dir = init(tag);
    write(&dir, "only_blank", "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n");
    write(&dir, "mixed", "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\n");
    commit(&dir, &[]);
    write(&dir, "only_blank", "one\ntwo\n\nthree\nfour\n\n\nfive\nsix\nseven\neight\n");
    write(&dir, "mixed", "alpha\nbeta\n\ngamma\ndeltaX\nepsilon\nzeta\neta\ntheta\n");
    dir
}

/// `--ignore-blank-lines` marks an all-blank change group ignorable, which drops
/// the whole file from the patch — header included, because `builtin_diff()` only
/// flushes `ecbdata.header` once a hunk line arrives.
#[test]
fn diff_files_ignore_blank_lines_drops_the_all_blank_pair() {
    let dir = blank_fixture("ibl");
    let got = git(&dir, &["diff-files", "-p", "--ignore-blank-lines"]);
    assert!(!got.contains("only_blank"), "all-blank pair still rendered:\n{got}");
    assert!(got.contains("diff --git a/mixed b/mixed\n"), "real change lost:\n{got}");

    // Not one of `XDF_WHITESPACE_FLAGS`, so `diff_from_contents` stays clear and
    // the raw listing keeps both pairs.
    let raw = git(&dir, &["diff-files", "--raw", "--ignore-blank-lines"]);
    assert_eq!(raw.lines().count(), 2, "raw listing lost a pair:\n{raw}");
    assert!(raw.contains("\tonly_blank\n") && raw.contains("\tmixed\n"), "{raw}");
}

/// The counts come from what `xdl_emit_diff()` printed, so an ignorable group a
/// neighbouring live group pulled into its hunk is still counted — `2 1`, not the
/// `1 1` that summing the live groups gives.
#[test]
fn diff_files_ignore_blank_lines_counts_what_the_hunk_printed() {
    let dir = blank_fixture("iblstat");
    assert_eq!(
        git(&dir, &["diff-files", "--stat", "--ignore-blank-lines"]),
        " mixed | 3 ++-\n 1 file changed, 2 insertions(+), 1 deletion(-)\n"
    );
    // With no context the two groups are separate hunks and the blank one is
    // dropped, counts included. (`-U<n>` is `diff_opt_unified()`, which also turns
    // the patch on, so stock prints the hunk after the stat.)
    let got = git(&dir, &["diff-files", "--stat", "--ignore-blank-lines", "-U0"]);
    assert!(
        got.starts_with(" mixed | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n"),
        "-U0 must drop the standalone blank group from the counts:\n{got}"
    );
}

/// `xdl_mark_ignorable_regex()` opens with "Do not override
/// --ignore-blank-lines", so the two markers are an OR: a group either predicate
/// accepts is ignorable.
#[test]
fn diff_files_ignore_blank_lines_and_regex_are_or_ed() {
    let dir = blank_fixture("iblre");
    // `-I gamma` alone matches nothing in `only_blank`, so on its own it keeps the
    // file; together with `--ignore-blank-lines` the blank rule still drops it.
    let only_re = git(&dir, &["diff-files", "-p", "-I", "gamma"]);
    assert!(only_re.contains("only_blank"), "-I alone must keep the all-blank pair:\n{only_re}");

    let both = git(&dir, &["diff-files", "-p", "--ignore-blank-lines", "-I", "gamma"]);
    assert!(!both.contains("only_blank"), "-I must not override --ignore-blank-lines:\n{both}");
}
