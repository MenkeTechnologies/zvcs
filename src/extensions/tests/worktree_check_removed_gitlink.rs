//! `check_removed()` (diff-lib.c:42) and the filespec shapes that hang off it.
//!
//! gix's index↔worktree comparison calls an entry *removed* whenever its path
//! `lstat`s as a directory. git only agrees when that directory is not a repository:
//! when it is one, `ce_mode_from_stat()` gives the pair `S_IFGITLINK` and the change is
//! a type change to `160000`, not a deletion. Every consumer of gix's verdict has to
//! re-ask, and this file pins the answer in `diff-files` and in all four `status` views
//! at once, since a fix that reaches one view and not the others is the failure mode.
//!
//! It also pins the two filespec shapes the same directory play breaks:
//!
//! * a *deletion* whose name is now a directory, or is still readable through a
//!   symlinked leading path — git's post-image is the invalid filespec (no content,
//!   never a worktree read), where reading the path either dies with
//!   `Is a directory (os error 21)` or silently succeeds and swallows the hunk;
//! * a *gitlink* on either side, which cannot go through the blob pipeline at all and
//!   renders as `diff_populate_gitlink()`'s one-line `Subproject commit <oid>` image.
//!
//! Every expectation below was measured from stock git 2.55.0.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// PATH with any zvcs shadow dir removed, so a `git` sub-invocation during setup
/// resolves to the real system git rather than recursing through the shadow.
fn real_git_path() -> String {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|p| !p.contains(".zvcs"))
        .collect::<Vec<_>>()
        .join(":")
}

fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args([
            "-c", "user.email=t@e.x",
            "-c", "user.name=t",
            "-c", "commit.gpgsign=false",
            "-c", "protocol.file.allow=always",
        ])
        .args(args)
        .env("PATH", real_git_path())
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

/// Run and require success, returning stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

/// Run and require success, returning trimmed stdout — for single-value queries.
fn git1(dir: &Path, args: &[&str]) -> String {
    git(dir, args).trim_end().to_owned()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-checkremoved-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create scratch");
    root.canonicalize().expect("canonicalize scratch")
}

/// An initialised repository with one committed blob `f` containing `alpha\n`.
fn repo_with_blob(root: &Path, name: &str) -> std::path::PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("create repo dir");
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("f"), b"alpha\n").expect("write f");
    std::fs::write(dir.join("keep"), b"keep\n").expect("write keep");
    git(&dir, &["add", "f", "keep"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    dir
}

/// A repository at `dir` carrying a single commit, i.e. one whose `HEAD` resolves —
/// which is exactly what `resolve_gitlink_ref()` asks of a directory.
fn nested_repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create nested dir");
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("s"), b"sub\n").expect("write s");
    git(dir, &["add", "s"]);
    git(dir, &["commit", "-q", "-m", "sub"]);
}

/// A superproject with a real `.gitmodules` submodule at `sm`, committed.
fn repo_with_submodule(root: &Path, name: &str) -> std::path::PathBuf {
    let src = root.join(format!("{name}-src"));
    nested_repo(&src);
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("create superproject");
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("keep"), b"keep\n").expect("write keep");
    git(&dir, &["add", "keep"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["submodule", "add", "-q", src.to_str().unwrap(), "sm"]);
    git(&dir, &["commit", "-q", "-m", "addsub"]);
    dir
}

/// A tracked blob whose name a *checked-out repository* has taken is a type change to
/// `160000`, not a deletion — `check_removed()` returns 0 for it (diff-lib.c:72) and
/// `run_diff_files()` gives the pair `ce_mode_from_stat()`'s `S_IFGITLINK`.
#[test]
fn blob_replaced_by_a_repository_is_a_typechange_to_a_gitlink() {
    let root = scratch("gitlink");
    let repo = repo_with_blob(&root, "r");
    let blob = git1(&repo, &["rev-parse", ":f"]);

    std::fs::remove_file(repo.join("f")).expect("rm f");
    nested_repo(&repo.join("f"));
    let head = git1(&repo.join("f"), &["rev-parse", "HEAD"]);

    let zero = "0".repeat(blob.len());
    assert_eq!(
        git(&repo, &["diff-files", "--raw"]),
        format!(":100644 160000 {blob} {zero} T\tf\n"),
        "the post-image mode is S_IFGITLINK and its id stays null in --raw"
    );
    assert_eq!(git(&repo, &["diff-files", "--name-status"]), "T\tf\n");
    assert_eq!(
        git(&repo, &["diff-files", "--summary"]),
        " mode change 100644 => 160000 f\n"
    );
    // `builtin_diffstat()` is handed the mixed blob/gitlink pair whole, so the one-line
    // `Subproject commit` image is diffed against the one-line blob.
    assert_eq!(git(&repo, &["diff-files", "--numstat"]), "1\t1\tf\n");

    // `run_diff()` (diff.c:5052) renders a pair whose two sides differ in `S_IFMT` as a
    // deletion patch followed by a creation patch. Before the fix the whole command
    // died reading the directory.
    let patch = git(&repo, &["diff-files", "-p"]);
    let expected = format!(
        "diff --git a/f b/f\n\
         deleted file mode 100644\n\
         index {}..{}\n\
         --- a/f\n\
         +++ /dev/null\n\
         @@ -1 +0,0 @@\n\
         -alpha\n\
         diff --git a/f b/f\n\
         new file mode 160000\n\
         index {}..{}\n\
         --- /dev/null\n\
         +++ b/f\n\
         @@ -0,0 +1 @@\n\
         +Subproject commit {head}\n",
        &blob[..10],
        &zero[..10],
        &zero[..10],
        &head[..10],
    );
    assert_eq!(patch, expected, "type change splits into deletion + creation");

    // All four status views. `--short` and `--porcelain` disagree on purpose:
    // `short_submodule_status()` (wt-status.c:449) only runs for `STATUS_FORMAT_SHORT`,
    // and the worktree gitlink's id is null, so `new_submodule_commits` is set and the
    // short letter is `M` while the porcelain letter stays `T`.
    assert_eq!(git(&repo, &["status", "--short"]), " M f\n");
    assert_eq!(git(&repo, &["status", "--porcelain"]), " T f\n");
    assert_eq!(
        git(&repo, &["status", "--porcelain=v2"]),
        format!("1 .T SC.. 100644 100644 160000 {blob} {blob} f\n"),
        "the <sub> column is S plus the new-commits flag"
    );
    let long = git(&repo, &["status"]);
    assert!(
        long.contains("\ttypechange: f (new commits)\n"),
        "long status names the type change and its submodule note: {long}"
    );
    assert!(
        !long.contains("deleted:"),
        "the blob was not deleted: {long}"
    );
}

/// The same shape with a *plain* directory: `check_removed()` returns 1, so it really
/// is a deletion — and the patch must still render, which means the post-image is git's
/// invalid filespec rather than a read of the directory that took the name.
#[test]
fn blob_replaced_by_a_plain_directory_is_a_deletion_that_still_renders() {
    let root = scratch("plaindir");
    let repo = repo_with_blob(&root, "r");
    let blob = git1(&repo, &["rev-parse", ":f"]);

    std::fs::remove_file(repo.join("f")).expect("rm f");
    std::fs::create_dir(repo.join("f")).expect("mkdir f");
    std::fs::write(repo.join("f/inner"), b"inner\n").expect("write inner");

    let zero = "0".repeat(blob.len());
    assert_eq!(
        git(&repo, &["diff-files", "--raw"]),
        format!(":100644 000000 {blob} {zero} D\tf\n")
    );
    assert_eq!(
        git(&repo, &["diff-files", "-p"]),
        format!(
            "diff --git a/f b/f\n\
             deleted file mode 100644\n\
             index {}..{}\n\
             --- a/f\n\
             +++ /dev/null\n\
             @@ -1 +0,0 @@\n\
             -alpha\n",
            &blob[..10],
            &zero[..10],
        ),
        "the deletion renders instead of dying on `Is a directory`"
    );
    assert_eq!(git(&repo, &["diff-files", "--summary"]), " delete mode 100644 f\n");

    // `index_name_is_other()` (read-cache.c:3442) strips the trailing `/` before the
    // lookup, so the collapsed directory entry is dropped: the index still holds `f`.
    assert_eq!(git(&repo, &["status", "--porcelain"]), " D f\n");
    assert_eq!(git(&repo, &["status", "--short"]), " D f\n");
    assert_eq!(
        git(&repo, &["status", "--porcelain=v2"]),
        format!("1 .D N... 100644 100644 000000 {blob} {blob} f\n")
    );
    // With `-uall` the files *inside* are offered under their own names, which the
    // index does not hold, so they survive the same filter.
    assert_eq!(
        git(&repo, &["status", "--porcelain", "-uall"]),
        " D f\n?? f/inner\n"
    );
}

/// A gitlink pair cannot go through the blob pipeline: `diff_populate_gitlink()`
/// (diff.c:4470) synthesises a one-line image for each present side.
#[test]
fn removed_submodule_renders_a_subproject_deletion() {
    let root = scratch("submodule");
    let repo = repo_with_submodule(&root, "sup");
    let head = git1(&repo, &["rev-parse", ":sm"]);
    std::fs::remove_dir_all(repo.join("sm")).expect("rm -r sm");

    let zero = "0".repeat(head.len());
    assert_eq!(
        git(&repo, &["diff-files", "--raw"]),
        format!(":160000 000000 {head} {zero} D\tsm\n")
    );
    assert_eq!(
        git(&repo, &["diff-files", "-p"]),
        format!(
            "diff --git a/sm b/sm\n\
             deleted file mode 160000\n\
             index {}..{}\n\
             --- a/sm\n\
             +++ /dev/null\n\
             @@ -1 +0,0 @@\n\
             -Subproject commit {head}\n",
            &head[..10],
            &zero[..10],
        ),
        "the pre-image is the synthetic Subproject line, not a blob read"
    );
    assert_eq!(git(&repo, &["diff-files", "--numstat"]), "0\t1\tsm\n");
    assert_eq!(
        git(&repo, &["status", "--porcelain=v2"]),
        format!("1 .D S... 160000 160000 000000 {head} {head} sm\n"),
        "a gitlink mode anywhere in the record makes the <sub> column S"
    );
}

/// `short_submodule_status()` (wt-status.c:449) is applied by
/// `wt_status_collect_changed_cb()` only when the format is `STATUS_FORMAT_SHORT`, so
/// `--short` and `--porcelain` deliberately print different letters for the same
/// worktree. The long format spells the same two bits out instead.
#[test]
fn submodule_dirtiness_letters_split_short_from_porcelain() {
    let root = scratch("dirtysub");

    // Tracked content modified inside the submodule: `m` short, `M` porcelain.
    let modified = repo_with_submodule(&root, "mod");
    std::fs::write(modified.join("sm/s"), b"sub\nlocal\n").expect("dirty the submodule");
    assert_eq!(git(&modified, &["status", "--short"]), " m sm\n");
    assert_eq!(git(&modified, &["status", "--porcelain"]), " M sm\n");
    let head = git1(&modified, &["rev-parse", ":sm"]);
    assert_eq!(
        git(&modified, &["status", "--porcelain=v2"]),
        format!("1 .M S.M. 160000 160000 160000 {head} {head} sm\n")
    );
    let long = git(&modified, &["status"]);
    assert!(
        long.contains("\tmodified:   sm (modified content)\n"),
        "the long format spells out d->dirty_submodule: {long}"
    );
    assert!(
        long.contains("  (commit or discard the untracked or modified content in submodules)\n"),
        "and raises the extra dirty-submodule hint: {long}"
    );

    // Untracked content only: `?` short, `M` porcelain, `U` in the <sub> column.
    let untracked = repo_with_submodule(&root, "untr");
    std::fs::write(untracked.join("sm/new"), b"new\n").expect("untracked in submodule");
    assert_eq!(git(&untracked, &["status", "--short"]), " ? sm\n");
    assert_eq!(git(&untracked, &["status", "--porcelain"]), " M sm\n");
    let head = git1(&untracked, &["rev-parse", ":sm"]);
    assert_eq!(
        git(&untracked, &["status", "--porcelain=v2"]),
        format!("1 .M S..U 160000 160000 160000 {head} {head} sm\n")
    );
    assert!(
        git(&untracked, &["status"]).contains("\tmodified:   sm (untracked content)\n"),
        "untracked content is its own note"
    );

    // Both at once: the bits are independent, and the long format lists them in git's
    // fixed order.
    let both = repo_with_submodule(&root, "both");
    std::fs::write(both.join("sm/s"), b"sub\nlocal\n").expect("dirty the submodule");
    std::fs::write(both.join("sm/new"), b"new\n").expect("untracked in submodule");
    let head = git1(&both, &["rev-parse", ":sm"]);
    assert_eq!(git(&both, &["status", "--short"]), " m sm\n");
    assert_eq!(
        git(&both, &["status", "--porcelain=v2"]),
        format!("1 .M S.MU 160000 160000 160000 {head} {head} sm\n")
    );
    assert!(
        git(&both, &["status"]).contains("\tmodified:   sm (modified content, untracked content)\n")
    );

    // A moved submodule outranks both: `new_submodule_commits` short-circuits
    // `short_submodule_status()` back to `M`.
    let moved = repo_with_submodule(&root, "moved");
    std::fs::write(moved.join("sm/s"), b"sub\nnext\n").expect("write in submodule");
    git(&moved.join("sm"), &["commit", "-q", "-am", "next"]);
    assert_eq!(git(&moved, &["status", "--short"]), " M sm\n");
    assert!(
        git(&moved, &["status"]).contains("\tmodified:   sm (new commits)\n"),
        "a moved submodule is reported as new commits"
    );
}

/// `is_submodule_modified()`'s middle clause (submodule.c:1931):
///
/// ```c
/// if (buf.buf[5] == 'S' && buf.buf[8] == 'U')   /* nested untracked file */
///         dirty_submodule |= DIRTY_SUBMODULE_UNTRACKED;
/// if (buf.buf[0] == 'u' || buf.buf[0] == '2' || memcmp(buf.buf + 5, "S..U", 4))
///         dirty_submodule |= DIRTY_SUBMODULE_MODIFIED;
/// ```
///
/// A submodule is "untracked-dirty" when its *own* submodule holds untracked files,
/// and that alone must not make it "modified" — the whole `<sub>` column has to be
/// exactly `S..U` for the second clause to stay silent. Reading the classification off
/// a flat list of changes gets this backwards and reports `m` / `S.M.` /
/// `(modified content)`, which is what a superproject of superprojects shows on every
/// line.
#[test]
fn nested_untracked_content_propagates_without_becoming_modified() {
    let root = scratch("nested");

    // inner <- mid <- outer, each a real `.gitmodules` submodule of the next.
    let inner = root.join("inner");
    nested_repo(&inner);

    let mid = root.join("mid");
    std::fs::create_dir_all(&mid).expect("create mid");
    git(&mid, &["init", "-q", "-b", "main"]);
    std::fs::write(mid.join("m"), b"m\n").expect("write m");
    git(&mid, &["add", "m"]);
    git(&mid, &["commit", "-q", "-m", "m"]);
    git(&mid, &["submodule", "add", "-q", inner.to_str().unwrap(), "sub"]);
    git(&mid, &["commit", "-q", "-m", "add inner"]);

    let outer = root.join("outer");
    std::fs::create_dir_all(&outer).expect("create outer");
    git(&outer, &["init", "-q", "-b", "main"]);
    std::fs::write(outer.join("o"), b"o\n").expect("write o");
    git(&outer, &["add", "o"]);
    git(&outer, &["commit", "-q", "-m", "o"]);
    git(&outer, &["submodule", "add", "-q", mid.to_str().unwrap(), "mid"]);
    git(&outer, &["commit", "-q", "-m", "add mid"]);
    git(&outer, &["submodule", "update", "--init", "--recursive"]);

    assert_eq!(
        git(&outer, &["status", "--porcelain=v2"]),
        "",
        "a freshly checked-out nesting is clean"
    );

    // One untracked file, two levels down.
    std::fs::write(outer.join("mid/sub/untr"), b"u\n").expect("untracked two levels down");

    let head = git1(&outer, &["rev-parse", ":mid"]);
    assert_eq!(
        git(&outer, &["status", "--porcelain=v2"]),
        format!("1 .M S..U 160000 160000 160000 {head} {head} mid\n"),
        "the U bit crosses the nesting and the M bit does not"
    );
    assert_eq!(
        git(&outer, &["status", "--short"]),
        " ? mid\n",
        "short_submodule_status() reaches its untracked arm, not its modified one"
    );
    assert!(
        git(&outer, &["status"]).contains("\tmodified:   mid (untracked content)\n"),
        "and the long format says untracked content alone"
    );
}

/// `check_removed()` also returns 1 for `has_symlink_leading_path()` (diff-lib.c:56).
/// gix reports that as `Change::Removed` too, but the file is still *readable* through
/// the symlink — so a post-image that reads the path silently produces an empty diff
/// and the deletion loses its hunk.
#[cfg(unix)]
#[test]
fn deletion_behind_a_symlinked_leading_path_keeps_its_hunk() {
    let root = scratch("symlinkpath");
    let repo = root.join("r");
    std::fs::create_dir_all(repo.join("d")).expect("create d");
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("d/f"), b"alpha\n").expect("write d/f");
    git(&repo, &["add", "d"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let blob = git1(&repo, &["rev-parse", ":d/f"]);

    // `d` becomes a symlink to a directory holding the very same bytes at the very
    // same name, so a naive worktree read finds `alpha\n` and reports no change.
    std::fs::remove_dir_all(repo.join("d")).expect("rm -r d");
    std::fs::create_dir(repo.join("elsewhere")).expect("mkdir elsewhere");
    std::fs::write(repo.join("elsewhere/f"), b"alpha\n").expect("write elsewhere/f");
    std::os::unix::fs::symlink("elsewhere", repo.join("d")).expect("symlink d");

    let zero = "0".repeat(blob.len());
    assert_eq!(
        git(&repo, &["diff-files", "-p"]),
        format!(
            "diff --git a/d/f b/d/f\n\
             deleted file mode 100644\n\
             index {}..{}\n\
             --- a/d/f\n\
             +++ /dev/null\n\
             @@ -1 +0,0 @@\n\
             -alpha\n",
            &blob[..10],
            &zero[..10],
        ),
        "the post-image is the invalid filespec, not a read through the symlink"
    );
}

/// The `S_IFMT` split is not gitlink-specific: a blob that became a symlink is two
/// patch sections as well, while the raw and summary formats keep one record.
#[cfg(unix)]
#[test]
fn blob_replaced_by_a_symlink_splits_the_patch() {
    let root = scratch("symlink");
    let repo = repo_with_blob(&root, "r");
    let blob = git1(&repo, &["rev-parse", ":f"]);

    std::fs::remove_file(repo.join("f")).expect("rm f");
    std::os::unix::fs::symlink("keep", repo.join("f")).expect("symlink f");

    assert_eq!(git(&repo, &["diff-files", "--name-status"]), "T\tf\n");
    assert_eq!(
        git(&repo, &["diff-files", "--summary"]),
        " mode change 100644 => 120000 f\n"
    );
    let patch = git(&repo, &["diff-files", "-p"]);
    assert!(
        patch.starts_with(&format!(
            "diff --git a/f b/f\ndeleted file mode 100644\nindex {}..",
            &blob[..10]
        )),
        "the first section is the deletion: {patch}"
    );
    assert!(
        patch.contains("\n-alpha\ndiff --git a/f b/f\nnew file mode 120000\n"),
        "the second section is the creation: {patch}"
    );
    assert!(
        patch.ends_with("+keep\n\\ No newline at end of file\n"),
        "the symlink's target is its content: {patch}"
    );
    assert!(
        !patch.contains("old mode 100644"),
        "a type change is never an `old mode`/`new mode` pair: {patch}"
    );
}
