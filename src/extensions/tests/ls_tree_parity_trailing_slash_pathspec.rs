//! Five measured divergences against stock git 2.55.0: four in how `ls-tree`
//! turns its operands into a pathspec, one in how `commit-tree` completes a
//! message line.
//!
//! 1. A pathspec longer than the entry it is tested against is
//!    `match_entry()`'s "partial pathname" case, and a trailing `/` is exactly
//!    what makes `a/` longer than `a`:
//!
//! ```c
//!     if (matchlen > pathlen) {
//!             if (match[pathlen] != '/')
//!                     return 0;
//!             /*
//!              * Reject non-directories as partial pathnames, except
//!              * when match is a submodule with a trailing slash and
//!              * nothing else (to handle 'submod/' and 'submod'
//!              * uniformly).
//!              */
//!             if (!S_ISDIR(entry->mode) &&
//!                 (!S_ISGITLINK(entry->mode) || matchlen > pathlen + 1))
//!                     return 0;
//!     }
//! ```
//!
//!    (tree-walk.c:892-904, reached from `do_match()` :1090 for the pathspec
//!    `ls-tree` builds at builtin/ls-tree.c:420-426.) The port stripped the
//!    trailing slash before comparing, so `git ls-tree HEAD a/` listed the blob
//!    `a` where stock lists nothing, and `symlink/` listed the symlink.
//!
//!    Measured, stock git 2.55.0:
//!
//! ```text
//!     $ git ls-tree HEAD a/
//!     $ git ls-tree -r HEAD symlink/
//!     $ git ls-tree HEAD deep/
//!     100644 blob 6f18529…    deep/a
//!     040000 tree 7a1fce5…    deep/deeper1
//! ```
//!
//! 2. `-m ""` leaves the message empty rather than making it one newline:
//!
//! ```c
//!     if (buf->len)
//!             strbuf_addch(buf, '\n');
//!     strbuf_addstr(buf, arg);
//!     strbuf_complete_line(buf);
//! ```
//!
//!    (`parse_message_arg_callback()`, builtin/commit-tree.c:64-67.)
//!    `strbuf_complete_line()` tests `sb->len` first, so it adds nothing to an
//!    empty buffer. The port completed the line unconditionally, producing a
//!    commit object one byte longer — and therefore a different object id —
//!    than stock's for the same inputs.
//!
//! 3. `parse_pathspec()` runs between naming the tree-ish and parsing the tree
//!    (builtin/ls-tree.c:410-423, :427-429). The port parsed the operands while
//!    collecting them, so a rejected pathspec outran `Not a valid object name`.
//!
//! 4. `init_pathspec_item()` dies on an element that leaves the working tree;
//!    the port normalized such an element into a harmless no-match and exited 0.
//!
//! 5. `abspath_part_inside_repo()` (setup.c:56-118) respells an absolute element
//!    that *is* inside the working tree relative to its root; the port appended
//!    it to the cwd prefix, so it matched nothing.
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
    /// A tree holding one blob, one symlink, one directory and one gitlink —
    /// the four entry kinds the trailing-slash rule distinguishes.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-lst-slash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(work.join("deep")).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        std::fs::write(f.work.join("deep/x"), "x\n").unwrap();
        std::os::unix::fs::symlink("a", f.work.join("symlink")).unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "initial"]);
        // A gitlink needs no populated submodule to sit in a tree.
        let head = f.stdout(&["rev-parse", "HEAD"]);
        f.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},submod", head.trim()),
        ]);
        f.git(&["commit", "-q", "-m", "gitlink"]);
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

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The `<path>` column of every line, which is all these cases turn on.
    fn names(&self, args: &[&str]) -> Vec<String> {
        self.stdout(args)
            .lines()
            .map(|l| l.split('\t').nth(1).unwrap_or_default().to_owned())
            .collect()
    }
}

/// A trailing slash demands a directory: a blob and a symlink match nothing.
#[test]
fn ls_tree_trailing_slash_rejects_a_non_directory() {
    let f = Fixture::new("blob");

    assert_eq!(f.names(&["ls-tree", "HEAD", "a"]), vec!["a"]);
    assert_eq!(
        f.names(&["ls-tree", "HEAD", "a/"]),
        Vec::<String>::new(),
        "`a/` matched the blob `a`"
    );
    assert_eq!(
        f.names(&["ls-tree", "-r", "HEAD", "a/"]),
        Vec::<String>::new(),
        "`a/` matched the blob `a` under -r"
    );
    assert_eq!(
        f.names(&["ls-tree", "HEAD", "symlink/"]),
        Vec::<String>::new(),
        "`symlink/` matched the symlink"
    );
    // `-t -r` still prints the ancestor tree the spec points into; what the
    // trailing slash removes is the blob `deep/x` itself.
    assert_eq!(
        f.names(&["ls-tree", "-t", "-r", "HEAD", "deep/x/"]),
        vec!["deep"],
        "a trailing slash on a nested blob still matched it"
    );
}

/// The tree arm of the same rule is unchanged: `deep/` selects the directory,
/// and the trailing slash is what makes `ls-tree` descend instead of printing
/// the directory's own line.
#[test]
fn ls_tree_trailing_slash_still_descends_into_a_directory() {
    let f = Fixture::new("tree");

    assert_eq!(f.names(&["ls-tree", "HEAD", "deep"]), vec!["deep"]);
    assert_eq!(
        f.names(&["ls-tree", "HEAD", "deep/"]),
        vec!["deep/x"],
        "`deep/` did not descend"
    );
    // Repeated slashes are still one partial-pathname boundary.
    assert_eq!(f.names(&["ls-tree", "HEAD", "deep//"]), vec!["deep/x"]);
    // `-t` brings the suppressed directory line back.
    assert_eq!(
        f.names(&["ls-tree", "-t", "HEAD", "deep/"]),
        vec!["deep", "deep/x"]
    );
}

/// The gitlink exception, spelled out in the C comment: `submod/` is the same
/// as `submod`, while a path *below* it is not.
#[test]
fn ls_tree_trailing_slash_treats_a_gitlink_like_its_bare_name() {
    let f = Fixture::new("gitlink");

    assert_eq!(f.names(&["ls-tree", "HEAD", "submod"]), vec!["submod"]);
    assert_eq!(
        f.names(&["ls-tree", "HEAD", "submod/"]),
        vec!["submod"],
        "`submod/` lost the gitlink"
    );
    // A repeated slash is collapsed by the pathspec normalization long before
    // `match_entry()` counts bytes, so `submod//` is `submod/`.
    assert_eq!(
        f.names(&["ls-tree", "HEAD", "submod//"]),
        vec!["submod"],
        "`submod//` lost the gitlink"
    );
    assert_eq!(
        f.names(&["ls-tree", "HEAD", "submod/x"]),
        Vec::<String>::new(),
        "a path below a gitlink matched"
    );
}

/// An empty `-m` contributes nothing, so the commit body stays empty and the
/// object id is stock's.
#[test]
fn commit_tree_empty_message_adds_no_newline() {
    let f = Fixture::new("emptymsg");
    let tree = f.stdout(&["write-tree"]);
    let tree = tree.trim();

    let id = f.stdout(&["commit-tree", "-m", "", tree]);
    let body = f.stdout(&["cat-file", "commit", id.trim()]);
    let (_, message) = body.split_once("\n\n").expect("no header/body separator");
    assert_eq!(message, "", "an empty -m produced a body: {message:?}");

    // Between two messages the separator is still added, and the first one's
    // line is still completed.
    let id = f.stdout(&["commit-tree", "-m", "one", "-m", "", tree]);
    let body = f.stdout(&["cat-file", "commit", id.trim()]);
    let (_, message) = body.split_once("\n\n").expect("no header/body separator");
    assert_eq!(message, "one\n\n", "wrong body for `-m one -m \"\"`");

    let id = f.stdout(&["commit-tree", "-m", "", "-m", "two", tree]);
    let body = f.stdout(&["cat-file", "commit", id.trim()]);
    let (_, message) = body.split_once("\n\n").expect("no header/body separator");
    assert_eq!(message, "two\n", "wrong body for `-m \"\" -m two`");
}

/// `parse_pathspec()` sits between naming the tree-ish and parsing the tree
/// (builtin/ls-tree.c:410-423 and :427-429), so an unnameable `<tree-ish>`
/// outranks a rejected pathspec, and a rejected pathspec outranks `not a tree
/// object`. The port parsed the operands while collecting them, which put the
/// pathspec diagnostic first.
#[test]
fn ls_tree_names_the_tree_before_it_parses_the_pathspec() {
    let f = Fixture::new("order");

    let out = f.cmd(&["ls-tree", "nosuchname", ":(icase)x"]).output().unwrap();
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: Not a valid object name nosuchname\n",
        "the pathspec was parsed before the tree-ish was named"
    );

    // A full-length hex id *names* an object without the odb being consulted, so
    // the blob's id resolves and the pathspec is reached before
    // `repo_parse_tree_indirect()` can say `not a tree object`.
    let blob = f.stdout(&["rev-parse", "HEAD:a"]);
    let out = f
        .cmd(&["ls-tree", blob.trim(), ":(icase)x"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "fatal: :(icase)x: pathspec magic not supported by this command: 'icase'\n",
        "`not a tree object` outran the pathspec"
    );
}

/// `init_pathspec_item()` dies on an element that leaves the working tree,
/// which this port used to normalize into a harmless no-match.
#[test]
fn ls_tree_refuses_a_pathspec_outside_the_repository() {
    let f = Fixture::new("outside");
    let root = std::fs::canonicalize(&f.work).unwrap();
    let root = root.display();

    for spec in ["/a", "../x"] {
        let out = f.cmd(&["ls-tree", "HEAD", spec]).output().unwrap();
        assert_eq!(out.status.code(), Some(128), "`{spec}` was accepted");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("fatal: {spec}: '{spec}' is outside repository at '{root}'\n"),
            "wrong diagnostic for `{spec}`"
        );
    }

    // `:(top)` takes the path verbatim and is never tested against the prefix,
    // so the same text behind that magic is a silent no-match.
    let out = f.cmd(&["ls-tree", "HEAD", ":(top)/a"]).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        ":(top) was put through the prefix check: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");

    // The per-element order is the pathspec's own: the first bad element wins,
    // whichever of the two checks it trips.
    let out = f
        .cmd(&["ls-tree", "HEAD", ":(icase)x", "/a"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("pathspec magic not supported"),
        "the second element's failure was reported first"
    );
}

/// An absolute element that *is* inside the working tree is respelled relative
/// to its root rather than appended to the cwd prefix
/// (`abspath_part_inside_repo()`, setup.c:56-118). The port joined it to the
/// prefix, producing a path that matched nothing.
#[test]
fn ls_tree_absolute_pathspec_inside_the_worktree_is_root_relative() {
    let f = Fixture::new("abs");
    let root = std::fs::canonicalize(&f.work).unwrap();
    let abs = |rel: &str| root.join(rel).to_string_lossy().into_owned();

    assert_eq!(f.names(&["ls-tree", "HEAD", &abs("a")]), vec!["a"]);
    assert_eq!(f.names(&["ls-tree", "HEAD", &abs("deep")]), vec!["deep"]);
    // The trailing slash keeps its meaning through the respelling.
    assert_eq!(
        f.names(&["ls-tree", "HEAD", &format!("{}/", abs("deep"))]),
        vec!["deep/x"]
    );

    // From a subdirectory the element still means what it says, not
    // `deep/<the whole absolute path>`.
    let mut c = Command::new(BIN);
    let out = c
        .args(["ls-tree", "--full-name", "HEAD", &abs("a")])
        .current_dir(f.work.join("deep"))
        .env("HOME", &f.root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out.stdout).ends_with("\ta\n"),
        "an absolute element was resolved against the cwd prefix: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );

    // `..` inside the element is collapsed before the root is stripped.
    assert_eq!(
        f.names(&["ls-tree", "HEAD", &abs("deep/../a")]),
        vec!["a"],
        "`..` was not collapsed"
    );
}
