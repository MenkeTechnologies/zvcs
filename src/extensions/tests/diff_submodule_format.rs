//! `git diff` over a gitlink: the `--submodule[=<format>]` selector, and the
//! worktree side of a submodule that the plain patch has to describe.
//!
//! Regression: `diff` rejected `--submodule=short` outright (`unsupported option`,
//! exit 1) *and* dropped every index↔worktree gitlink change on the floor, so a
//! superproject whose submodule had moved or gone dirty printed nothing at all —
//! no patch, no `--raw` row, no `--stat` line, exit 0. Both are what a repository
//! that is a shell of submodules sees on literally every `git diff`.
//!
//! Every expectation below is stock git 2.55.0's, byte for byte.

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
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn temp_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-subfmt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

/// A superproject with `sub` committed at the newest of three submodule commits.
/// Returns the parent, and the submodule's commits oldest first.
fn fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, [String; 3]) {
    let root = temp_root(tag);
    let src = root.join("sub_src");
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main"]);
    let mut ids = Vec::new();
    for (n, body) in [("s1", "1\n"), ("s2", "1\n2\n"), ("s3", "1\n2\n3\n")] {
        std::fs::write(src.join("f"), body).unwrap();
        git(&src, &["add", "f"]);
        git(&src, &["commit", "-q", "-m", n]);
        ids.push(git(&src, &["rev-parse", "HEAD"]).trim().to_string());
    }

    let parent = root.join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    git(&parent, &["init", "-q", "-b", "main"]);
    std::fs::write(parent.join("seed"), b"seed\n").unwrap();
    git(&parent, &["add", "seed"]);
    git(&parent, &["commit", "-q", "-m", "seed"]);
    git(&parent, &["submodule", "add", "-q", src.to_str().unwrap(), "sub"]);
    git(&parent, &["commit", "-q", "-m", "add sub"]);

    (root, parent, [ids[0].clone(), ids[1].clone(), ids[2].clone()])
}

/// The `Submodule <path> <a>..<b>` header's two abbreviated ids, which are as wide
/// as `core.abbrev` decides and so are only ever checked as prefixes.
fn header_ids(line: &str) -> (String, String) {
    let rest = line.strip_prefix("Submodule sub ").unwrap_or_else(|| panic!("header: {line:?}"));
    let range = rest.split(&[' ', ':'][..]).next().unwrap();
    let (a, b) = range.split_once("..").unwrap_or_else(|| panic!("range: {range:?}"));
    (a.to_string(), b.trim_start_matches('.').to_string())
}

/// A submodule whose worktree moved to another commit is a one-line patch against
/// the index — the change that used to render as nothing at all.
#[test]
fn worktree_gitlink_bump_renders_the_short_patch() {
    let (root, parent, ids) = fixture("bump");
    git(&parent.join("sub"), &["checkout", "-q", &ids[0]]);

    assert_eq!(
        git(&parent, &["-c", "core.abbrev=10", "diff"]),
        format!(
            "diff --git a/sub b/sub\nindex {a}..{b} 160000\n--- a/sub\n+++ b/sub\n\
             @@ -1 +1 @@\n-Subproject commit {full_a}\n+Subproject commit {full_b}\n",
            a = &ids[2][..10],
            b = &ids[0][..10],
            full_a = ids[2],
            full_b = ids[0],
        )
    );
    // `run_diff_files()` leaves the post-image invalid once the gitlink moved, so
    // the raw record's second id is all-zero even though the patch names it.
    assert_eq!(
        git(&parent, &["diff", "--raw", "--abbrev=40"]),
        format!(":160000 160000 {} {} M\tsub\n", ids[2], "0".repeat(40))
    );
    assert_eq!(
        git(&parent, &["diff", "--numstat"]),
        "1\t1\tsub\n",
        "a moved pointer is one line replaced"
    );
    // `--exit-code` has to see the change too.
    assert_eq!(run(&parent, &["diff", "--exit-code"]).status.code(), Some(1));

    let _ = std::fs::remove_dir_all(&root);
}

/// Local damage inside a submodule that is still on its recorded commit: the patch
/// gains git's `-dirty` marker, there is no `index` line (both ids are equal), and
/// the stat formats stay at zero because only the ids feed them.
#[test]
fn dirty_submodule_is_marked_in_the_patch_but_not_counted_in_the_stat() {
    let (root, parent, ids) = fixture("dirty");
    std::fs::write(parent.join("sub").join("f"), b"1\n2\n3\nlocal\n").unwrap();

    assert_eq!(
        git(&parent, &["diff"]),
        format!(
            "diff --git a/sub b/sub\n--- a/sub\n+++ b/sub\n@@ -1 +1 @@\n\
             -Subproject commit {c}\n+Subproject commit {c}-dirty\n",
            c = ids[2]
        )
    );
    assert_eq!(git(&parent, &["diff", "--numstat"]), "0\t0\tsub\n");
    assert_eq!(
        git(&parent, &["diff", "--shortstat"]),
        " 1 file changed, 0 insertions(+), 0 deletions(-)\n"
    );
    // The gitlink itself did not move, so its id survives into the raw record.
    assert_eq!(
        git(&parent, &["diff", "--raw", "--abbrev=40"]),
        format!(":160000 160000 {c} {c} M\tsub\n", c = ids[2])
    );
    // Untracked content alone is not damage: `diff_setup_done()` clears
    // `DIRTY_SUBMODULE_UNTRACKED` for every diff.
    std::fs::write(parent.join("sub").join("f"), b"1\n2\n3\n").unwrap();
    std::fs::write(parent.join("sub").join("stray"), b"x\n").unwrap();
    assert_eq!(git(&parent, &["diff"]), "");

    let _ = std::fs::remove_dir_all(&root);
}

/// A submodule directory removed from the worktree is a deletion, not silence.
#[test]
fn removed_submodule_directory_renders_a_deletion() {
    let (root, parent, ids) = fixture("gone");
    std::fs::remove_dir_all(parent.join("sub")).unwrap();

    assert_eq!(
        git(&parent, &["-c", "core.abbrev=10", "diff"]),
        format!(
            "diff --git a/sub b/sub\ndeleted file mode 160000\nindex {a}..{z}\n\
             --- a/sub\n+++ /dev/null\n@@ -1 +0,0 @@\n-Subproject commit {full}\n",
            a = &ids[2][..10],
            z = "0".repeat(10),
            full = ids[2],
        )
    );
    assert_eq!(git(&parent, &["diff", "--name-status"]), "D\tsub\n");

    let _ = std::fs::remove_dir_all(&root);
}

/// `--submodule` with no value is `log`, not `short` (`diff_opt_submodule()`), and
/// the commit list is the `--left-right --first-parent` walk between the two ends.
#[test]
fn bare_submodule_option_selects_the_log_format() {
    let (root, parent, ids) = fixture("log");
    git(&parent.join("sub"), &["checkout", "-q", &ids[0]]);

    let bare = git(&parent, &["diff", "--submodule"]);
    assert_eq!(bare, git(&parent, &["diff", "--submodule=log"]), "bare --submodule is =log");

    let mut lines = bare.lines();
    let header = lines.next().unwrap();
    let (a, b) = header_ids(header);
    assert!(ids[2].starts_with(&a) && ids[0].starts_with(&b), "header ids: {header:?}");
    // Walking backwards is a rewind, and its two commits print on the left side.
    assert!(header.ends_with(" (rewind):"), "header: {header:?}");
    assert_eq!(lines.next(), Some("  < s3"));
    assert_eq!(lines.next(), Some("  < s2"));
    assert_eq!(lines.next(), None);

    // `short` is the default rendering, so asking for it explicitly changes nothing.
    assert_eq!(git(&parent, &["diff", "--submodule=short"]), git(&parent, &["diff"]));

    let _ = std::fs::remove_dir_all(&root);
}

/// Damage inside the submodule prints its own line ahead of the header — and when
/// the gitlink itself has not moved, that line is the whole report.
#[test]
fn submodule_log_reports_modified_content_before_the_header() {
    let (root, parent, ids) = fixture("logdirty");
    std::fs::write(parent.join("sub").join("f"), b"1\n2\n3\nlocal\n").unwrap();
    assert_eq!(git(&parent, &["diff", "--submodule=log"]), "Submodule sub contains modified content\n");

    // With the pointer moved as well, the same line precedes a full summary.
    std::fs::write(parent.join("sub").join("f"), b"1\n2\n3\n").unwrap();
    git(&parent.join("sub"), &["checkout", "-q", &ids[0]]);
    std::fs::write(parent.join("sub").join("f"), b"1\nlocal\n").unwrap();
    let out = git(&parent, &["diff", "--submodule=log"]);
    let mut lines = out.lines();
    assert_eq!(lines.next(), Some("Submodule sub contains modified content"));
    assert!(lines.next().unwrap().starts_with("Submodule sub "), "header after the dirty line");
    assert_eq!(lines.next(), Some("  < s3"));

    let _ = std::fs::remove_dir_all(&root);
}

/// `--submodule=diff` runs the submodule's own diff and pipes it through with the
/// gitlink path glued onto both prefixes, so every path it names is reachable from
/// the superproject.
#[test]
fn inline_submodule_diff_prefixes_the_submodule_paths() {
    let (root, parent, ids) = fixture("inline");
    git(&parent.join("sub"), &["checkout", "-q", &ids[0]]);

    let out = git(&parent, &["diff", "--submodule=diff"]);
    let mut lines = out.lines();
    assert!(lines.next().unwrap().starts_with("Submodule sub "), "header first");
    assert_eq!(lines.next(), Some("diff --git a/sub/f b/sub/f"));
    assert!(out.contains("--- a/sub/f\n+++ b/sub/f\n"), "prefixed file lines: {out:?}");
    assert!(out.contains("\n-2\n-3\n"), "the submodule's own hunk: {out:?}");

    // `--src-prefix`/`--dst-prefix` reach the child, which is what keeps the two
    // sides of the inline patch consistent with the surrounding diff.
    let named = git(&parent, &["diff", "--submodule=diff", "--src-prefix=X/", "--dst-prefix=Y/"]);
    assert!(named.contains("diff --git X/sub/f Y/sub/f\n"), "custom prefixes: {named:?}");

    let _ = std::fs::remove_dir_all(&root);
}

/// A format name git cannot parse is a usage error, and the config spelling of the
/// same mistake is only a warning that leaves the default in place.
#[test]
fn unparsable_submodule_format_is_rejected_the_way_git_rejects_it() {
    let (root, parent, ids) = fixture("bad");
    git(&parent.join("sub"), &["checkout", "-q", &ids[0]]);

    let out = run(&parent, &["diff", "--submodule=bogus"]);
    assert_eq!(out.status.code(), Some(129), "usage error");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: failed to parse --submodule option parameter: 'bogus'\n"
    );
    assert!(out.stdout.is_empty(), "nothing is diffed after a usage error");

    // `git_diff_ui_config()` warns and keeps the format it already had.
    git(&parent, &["config", "diff.submodule", "bogus"]);
    let out = run(&parent, &["diff"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "warning: Unknown value for 'diff.submodule' config variable: 'bogus'\n"
    );
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("diff --git a/sub b/sub\n"));

    // A value it can parse is the default the command line then overrides.
    git(&parent, &["config", "diff.submodule", "log"]);
    assert_eq!(git(&parent, &["diff"]), git(&parent, &["diff", "--submodule=log"]));
    assert!(git(&parent, &["diff", "--submodule=short"]).starts_with("diff --git a/sub b/sub\n"));

    let _ = std::fs::remove_dir_all(&root);
}

/// The formats are per-pair, so a diff that touches a submodule and an ordinary
/// file has to keep both renderings in path order.
#[test]
fn submodule_summary_and_blob_patch_stay_in_path_order() {
    let (root, parent, ids) = fixture("mixed");
    git(&parent.join("sub"), &["checkout", "-q", &ids[0]]);
    std::fs::write(parent.join("seed"), b"seed\nmore\n").unwrap();

    let out = git(&parent, &["diff", "--submodule=log"]);
    let seed_at = out.find("diff --git a/seed b/seed").expect("blob section");
    let sub_at = out.find("Submodule sub ").expect("submodule section");
    assert!(seed_at < sub_at, "`seed` sorts before `sub`: {out:?}");
    // The blob patch is untouched by the submodule format.
    assert!(out[seed_at..].contains("@@ -1 +1,2 @@\n seed\n+more\n"), "{out:?}");

    let _ = std::fs::remove_dir_all(&root);
}
