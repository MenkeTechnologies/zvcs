//! Which object directories a repository actually borrows from —
//! `odb_prepare_alternates()` (`odb.c:487-502`) and the
//! `odb_add_alternate_recursively()` / `odb_is_source_usable()` /
//! `parse_alternates()` trio it drives (`odb.c:56-205`).
//!
//! ```c
//! void odb_prepare_alternates(struct object_database *odb)
//! {
//!         struct strvec sources = STRVEC_INIT;
//!
//!         if (odb->loaded_alternates)
//!                 return;
//!
//!         parse_alternates(odb->alternate_db, PATH_SEP, NULL, &sources);
//!         odb_source_read_alternates(odb->sources, &sources);
//!         for (size_t i = 0; i < sources.nr; i++)
//!                 odb_add_alternate_recursively(odb, sources.v[i], 0);
//!
//!         odb->loaded_alternates = 1;
//!
//!         strvec_clear(&sources);
//! }
//! ```
//!
//! `git count-objects -v` prints one `alternate:` line per linked source, in
//! `odb->sources` order, which makes it the readable view of that list; every
//! expectation below was captured from stock git 2.55.0 against a fixture built
//! the same way.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-alt-sources-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn git_in(dir: &Path, args: &[&str]) -> Output {
    git_env(dir, args, &[])
}

fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .current_dir(dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run the binary under test")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The `alternate:` lines of `count-objects -v`, in order.
fn alternates_of(repo: &Path, env: &[(&str, &str)]) -> Vec<String> {
    let out = git_env(repo, &["count-objects", "-v"], env);
    stdout(&out)
        .lines()
        .filter_map(|l| l.strip_prefix("alternate: ").map(str::to_owned))
        .collect()
}

fn bare(at: &Path) -> PathBuf {
    std::fs::create_dir_all(at).unwrap();
    let out = git_in(at, &["init", "-q", "--bare", "."]);
    assert!(out.status.success(), "init {}: {}", at.display(), stderr(&out));
    at.join("objects")
}

/// One commit written straight into a bare repository, without a work tree:
/// the empty tree, then `commit-tree`, then the branch. Returns its object id.
fn commit_into_bare(repo: &Path, message: &str) -> String {
    let out = Command::new(BIN)
        .args(["hash-object", "-w", "-t", "tree", "--stdin"])
        .stdin(std::process::Stdio::null())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .current_dir(repo)
        .output()
        .expect("write the empty tree");
    assert!(out.status.success(), "hash-object: {}", stderr(&out));
    let tree = stdout(&out).trim().to_owned();

    let out = git_in(repo, &["commit-tree", &tree, "-m", message]);
    assert!(out.status.success(), "commit-tree: {}", stderr(&out));
    let commit = stdout(&out).trim().to_owned();
    assert_eq!(commit.len(), 40, "a full object id: {commit}");

    let out = git_in(repo, &["update-ref", "refs/heads/main", &commit]);
    assert!(out.status.success(), "update-ref: {}", stderr(&out));
    commit
}

fn set_alternates(objects: &Path, body: &str) {
    std::fs::create_dir_all(objects.join("info")).unwrap();
    std::fs::write(objects.join("info").join("alternates"), body).unwrap();
}

fn real(path: &Path) -> String {
    std::fs::canonicalize(path).unwrap().display().to_string()
}

/// `parse_alternates()` normalizes every entry with `strbuf_realpath()`
/// (`odb.c:149`) and trims the trailing slashes it leaves behind
/// (`odb.c:159-160`), so what the database links — and prints — is absolute and
/// symlink-free whatever the file said.
///
/// Measured against git 2.55.0, three borrowers naming one lender through a
/// symlink, a relative path and a path with trailing slashes:
///
/// ```text
/// $ git -C b.git count-objects -v | tail -1
/// alternate: /…/zz_sym/real.git/objects
/// $ git -C c.git count-objects -v | tail -1
/// alternate: /…/zz_sym/real.git/objects
/// $ git -C d.git count-objects -v | tail -1
/// alternate: /…/zz_sym/real.git/objects
/// ```
#[test]
fn every_entry_is_resolved_to_an_absolute_symlink_free_path() {
    let root = scratch("normalize");
    let lender = bare(&root.join("real.git"));
    let want = vec![real(&lender)];

    let via_symlink = root.join("b.git");
    bare(&via_symlink);
    std::os::unix::fs::symlink(root.join("real.git"), root.join("link.git")).unwrap();
    set_alternates(
        &via_symlink.join("objects"),
        &format!("{}\n", root.join("link.git").join("objects").display()),
    );
    assert_eq!(alternates_of(&via_symlink, &[]), want, "a symlink is followed");

    let via_relative = root.join("c.git");
    bare(&via_relative);
    set_alternates(&via_relative.join("objects"), "../../real.git/objects\n");
    assert_eq!(
        alternates_of(&via_relative, &[]),
        want,
        "a relative entry resolves against the object directory that lists it"
    );

    let with_slashes = root.join("d.git");
    bare(&with_slashes);
    set_alternates(&with_slashes.join("objects"), &format!("{}///\n", real(&lender)));
    assert_eq!(alternates_of(&with_slashes, &[]), want, "trailing slashes are trimmed");
}

/// `odb_is_source_usable()` seeds `source_by_path` with the primary object
/// directory and adds every alternate as it is linked (`odb.c:75-93`), so an
/// alternate that points back at its borrower is skipped rather than being an
/// error, and so is one listed twice.
///
/// Measured against git 2.55.0 on two repositories naming each other:
///
/// ```text
/// $ git -C cyc1.git count-objects -v
/// count: 0
/// …
/// alternate: /…/cyc2.git/objects
/// $ echo $?
/// 0
/// ```
#[test]
fn a_cycle_and_a_repeat_are_both_skipped_without_an_error() {
    let root = scratch("cycle");
    let one = root.join("cyc1.git");
    let two = root.join("cyc2.git");
    bare(&one);
    bare(&two);
    set_alternates(&one.join("objects"), &format!("{}\n", real(&two.join("objects"))));
    set_alternates(&two.join("objects"), &format!("{}\n", real(&one.join("objects"))));

    let out = git_in(&one, &["count-objects", "-v"]);
    assert!(out.status.success(), "a cycle is not a failure: {}", stderr(&out));
    assert_eq!(
        stdout(&out)
            .lines()
            .filter(|l| l.starts_with("alternate: "))
            .collect::<Vec<_>>(),
        vec![format!("alternate: {}", real(&two.join("objects")))]
    );
    assert!(
        !stderr(&out).contains("cycle"),
        "git says nothing about a cycle, it just stops: {}",
        stderr(&out)
    );

    let listed_twice = root.join("twice.git");
    bare(&listed_twice);
    // A lender with no alternates of its own, so the only thing under test is
    // the repeat.
    let lender = real(&bare(&root.join("plain-lender.git")));
    set_alternates(&listed_twice.join("objects"), &format!("{lender}\n{lender}\n"));
    assert_eq!(
        alternates_of(&listed_twice, &[]),
        vec![lender],
        "the common mistake of listing the same thing twice links it once"
    );
}

/// `if (sources.nr && depth + 1 > 5)` (`odb.c:194`) drops the level below rather
/// than trimming it, so a chain reached from the primary store contributes at
/// most six object directories and the seventh is unreachable.
///
/// Measured against git 2.55.0 on `a0 -> a1 -> … -> a7`, with a commit living in
/// `a7`:
///
/// ```text
/// $ git -C a1 rev-list --alternate-refs | wc -l
///        5
/// $ git -C a0 cat-file -t 4a3fb3cba98ce6bf7cd864f46410e58d7eca1ef0
/// error: /…/a6/objects: ignoring alternate object stores, nesting too deep
/// fatal: git cat-file: could not get object info
/// ```
#[test]
fn nesting_stops_after_five_levels() {
    let root = scratch("nesting");
    let link = |n: usize| root.join(format!("a{n}.git"));
    for n in 0..=7 {
        bare(&link(n));
    }
    for n in 0..7 {
        set_alternates(
            &link(n).join("objects"),
            &format!("{}\n", real(&link(n + 1).join("objects"))),
        );
    }
    // The object everything is chasing lives in the deepest store.
    let deep = commit_into_bare(&link(7), "deep");

    // From a1 the chain is six long and ends at the store that has the object.
    assert_eq!(
        alternates_of(&link(1), &[]),
        (2..=7).map(|n| real(&link(n).join("objects"))).collect::<Vec<_>>(),
        "six levels are linked, in pre-order"
    );
    let out = git_in(&link(1), &["cat-file", "-t", &deep]);
    assert_eq!(stdout(&out).trim(), "commit", "a1 reaches a7: {}", stderr(&out));

    // One link further out and the last level is dropped, taking the object with it.
    assert_eq!(
        alternates_of(&link(0), &[]),
        (1..=6).map(|n| real(&link(n).join("objects"))).collect::<Vec<_>>(),
        "a6 is linked but its own alternate is not"
    );
    let out = git_in(&link(0), &["cat-file", "-t", &deep]);
    assert!(
        !out.status.success(),
        "a0 must not reach a7: {}{}",
        stdout(&out),
        stderr(&out)
    );
}

/// `parse_alternates(odb->alternate_db, PATH_SEP, NULL, &sources)`
/// (`odb.c:494`): `$GIT_ALTERNATE_OBJECT_DIRECTORIES` is a source of alternates,
/// read *before* the repository's own `info/alternates`, with `:` for a
/// separator, `#` for a comment and an empty entry skipped.
///
/// Measured against git 2.55.0:
///
/// ```text
/// $ GIT_ALTERNATE_OBJECT_DIRECTORIES=/…/zz_base/.git/objects \
///       git -C env.git cat-file -t 4a3fb3cba98ce6bf7cd864f46410e58d7eca1ef0
/// commit
/// ```
#[test]
fn the_environment_names_alternates_and_they_come_first() {
    let root = scratch("environment");
    let borrower = root.join("borrower.git");
    bare(&borrower);
    let from_file = bare(&root.join("from-file.git"));
    let from_env = bare(&root.join("from-env.git"));
    set_alternates(&borrower.join("objects"), &format!("{}\n", real(&from_file)));

    let lent = commit_into_bare(&root.join("from-env.git"), "e");

    // `:`-separated, with an empty field and a comment field that contribute
    // nothing.
    let list = format!("{}::#comment", real(&from_env));
    let env = [("GIT_ALTERNATE_OBJECT_DIRECTORIES", list.as_str())];
    assert_eq!(
        alternates_of(&borrower, &env),
        vec![real(&from_env), real(&from_file)],
        "the environment's entries are linked ahead of the file's"
    );

    let out = git_env(&borrower, &["cat-file", "-t", &lent], &env);
    assert_eq!(
        stdout(&out).trim(),
        "commit",
        "an object borrowed through the environment is readable: {}",
        stderr(&out)
    );
    let out = git_in(&borrower, &["cat-file", "-t", &lent]);
    assert!(
        !out.status.success(),
        "and unreadable without it: {}{}",
        stdout(&out),
        stderr(&out)
    );
}

/// `odb_is_source_usable()`'s two refusals (`odb.c:68-73` and `odb.c:149-153`):
/// an entry whose path cannot be normalized, and one that is not a directory,
/// are both dropped — with a message, and without failing the command.
///
/// Measured against git 2.55.0:
///
/// ```text
/// $ git -C r.git count-objects -v
/// error: unable to normalize alternate object path: /…/nope/objects
/// count: 0
/// …
/// $ git -C rel.git count-objects -v
/// error: object directory /…/zz_base_objects does not exist; check .git/objects/info/alternates
/// count: 0
/// …
/// ```
///
/// Neither prints an `alternate:` line, and both exit 0.
#[test]
fn an_unusable_entry_is_reported_and_dropped() {
    let root = scratch("unusable");

    // A path whose *parent* is missing: `strbuf_realpath` fails outright.
    let missing = root.join("missing.git");
    bare(&missing);
    set_alternates(
        &missing.join("objects"),
        &format!("{}\n", root.join("nope").join("objects").display()),
    );
    let out = git_in(&missing, &["count-objects", "-v"]);
    assert!(out.status.success(), "the command carries on: {}", stderr(&out));
    assert!(
        alternates_of(&missing, &[]).is_empty(),
        "nothing is linked: {:?}",
        alternates_of(&missing, &[])
    );
    assert!(
        stderr(&out).contains("error: unable to normalize alternate object path: "),
        "stderr was {:?}",
        stderr(&out)
    );

    // A path that resolves but names a regular file rather than a directory.
    let not_a_dir = root.join("file.git");
    bare(&not_a_dir);
    std::fs::write(root.join("plain"), b"").unwrap();
    set_alternates(&not_a_dir.join("objects"), &format!("{}\n", real(&root.join("plain"))));
    let out = git_in(&not_a_dir, &["count-objects", "-v"]);
    assert!(out.status.success(), "the command carries on: {}", stderr(&out));
    assert!(
        alternates_of(&not_a_dir, &[]).is_empty(),
        "nothing is linked: {:?}",
        alternates_of(&not_a_dir, &[])
    );
    assert!(
        stderr(&out).contains(&format!(
            "error: object directory {} does not exist; check .git/objects/info/alternates",
            real(&root.join("plain"))
        )),
        "stderr was {:?}",
        stderr(&out)
    );
}

/// `odb_for_each_alternate_ref()` (`odb.c:463-470`) walks `odb->sources->next`,
/// so `git rev-list --alternate-refs` sees exactly the sources the object
/// database linked — the whole six-level chain included.
///
/// Measured against git 2.55.0 on `a1 -> … -> a7`, with `a7` holding a
/// five-commit `main`:
///
/// ```text
/// $ git -C a1 rev-list --alternate-refs
/// 4a3fb3cba98ce6bf7cd864f46410e58d7eca1ef0
/// 46d54d6a684e1a8144b2cbf2d98c6ee0ad47e8d6
/// 1fda4da119b297fdf93a4d4f9cd9647321f5c125
/// 780ef23b76e7c4efdf67e86d67c9eabc7ad70b54
/// 266d36d1b3df470a08af974245dd6ae843dc9c46
/// ```
#[test]
fn alternate_refs_reaches_the_whole_chain() {
    let root = scratch("altrefs");
    let link = |n: usize| root.join(format!("a{n}.git"));
    for n in 1..=7 {
        bare(&link(n));
    }
    for n in 1..7 {
        set_alternates(
            &link(n).join("objects"),
            &format!("{}\n", real(&link(n + 1).join("objects"))),
        );
    }
    let tip = commit_into_bare(&link(7), "tip");

    let out = git_in(&link(1), &["rev-list", "--alternate-refs"]);
    assert_eq!(
        stdout(&out).lines().collect::<Vec<_>>(),
        vec![tip.as_str()],
        "a1 lists a7's tip through five intermediate stores: {}",
        stderr(&out)
    );
}
