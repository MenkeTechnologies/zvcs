//! The read side of the reftable backend through the public API of
//! `file::Store`: references, iteration and reflogs of a repository that stock
//! git wrote with `init --ref-format=reftable`, including a linked worktree.
//!
//! Every expectation is what stock git 2.56.0 reports for the same repository
//! (`for-each-ref --include-root-refs`, `reflog list`, `reflog exists`,
//! `rev-parse`, and the reflog files `refs migrate --ref-format=files` writes
//! for the same history), spelled out as literals.

#[path = "../../../extensions/tests/support/stock_git.rs"]
mod stock_git;

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use gix_ref::{
    FullName, Reference, Target,
    file::Store,
    store::{RefStorage, init::Options},
};
use gix_testtools::tempfile::TempDir;

const ONE: &str = "76a57ce2bf33b96e5824acab4b2827d4cddce759";
const TWO: &str = "46faaf1799ce1b8580b5789b07382449969704c8";
const TAG: &str = "5376fcb015e12fe19494cac12a0e610a652edbfd";
const NULL: &str = "0000000000000000000000000000000000000000";

/// The repository `R` with commits `one` and `two` on `main`, a branch `side`
/// and an annotated tag `v1` on `two`, and a linked worktree `wt` on `side`.
/// Then, with auto-compaction off so every write stays a table of its own:
///
/// * `refs/bisect/bad` in the main worktree, a per-worktree ref of the main stack;
/// * `refs/worktree/x` (on `one`) in `wt`, which lives in `wt`'s own stack;
/// * a branch `gone` created and deleted, leaving a tombstone over its ref and reflog;
/// * the reflog of `side` expired entirely, leaving only its existence marker.
struct Fixture {
    _tmp: TempDir,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Option<Fixture> {
        let Some(git) = stock_git::stock_git_at_least((2, 56, 0)) else {
            eprintln!("skipped: no stock git 2.56 to build the reftable fixture");
            return None;
        };
        let tmp = gix_testtools::tempfile::Builder::new()
            .prefix("reftable-read-")
            .tempdir()
            .expect("temp dir");
        let run = |dir: &Path, args: &[&str]| {
            let out = Command::new(git)
                .args(args)
                .current_dir(dir)
                .env("HOME", tmp.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TEST_REFTABLE_AUTOCOMPACTION", "false")
                .env("GIT_AUTHOR_NAME", "A U Thor")
                .env("GIT_AUTHOR_EMAIL", "author@example.com")
                .env("GIT_COMMITTER_NAME", "C O Mitter")
                .env("GIT_COMMITTER_EMAIL", "committer@example.com")
                .env("GIT_AUTHOR_DATE", "1112911993 -0700")
                .env("GIT_COMMITTER_DATE", "1112911993 -0700")
                .output()
                .expect("stock git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let root = tmp.path();
        let repo = root.join("R");
        let wt = root.join("wt");
        run(root, &["init", "-q", "-b", "main", "--ref-format=reftable", "R"]);
        run(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
        run(&repo, &["commit", "-q", "--allow-empty", "-m", "two"]);
        run(&repo, &["branch", "side"]);
        run(&repo, &["tag", "-a", "v1", "-m", "t"]);
        run(&repo, &["worktree", "add", "-q", "../wt", "side"]);
        run(&repo, &["update-ref", "refs/bisect/bad", "HEAD"]);
        run(&wt, &["update-ref", "refs/worktree/x", "HEAD~1"]);
        run(&repo, &["branch", "gone"]);
        run(&repo, &["branch", "-q", "-D", "gone"]);
        run(&repo, &["reflog", "expire", "--expire=all", "refs/heads/side"]);
        Some(Fixture { _tmp: tmp, repo })
    }

    fn git_dir(&self) -> PathBuf {
        self.repo.join(".git")
    }

    fn options() -> Options {
        Options {
            object_hash: gix_hash::Kind::Sha1,
            ref_storage: RefStorage::Reftable,
            ..Default::default()
        }
    }

    /// The store of the main worktree.
    fn main(&self) -> Store {
        Store::at(self.git_dir(), Self::options())
    }

    /// The store of the linked worktree `wt`.
    fn worktree(&self) -> Store {
        Store::for_linked_worktree(self.git_dir().join("worktrees/wt"), self.git_dir(), Self::options())
    }
}

fn id(hex: &str) -> gix_hash::ObjectId {
    gix_hash::ObjectId::from_hex(hex.as_bytes()).expect("valid hex")
}

fn full_name(name: &str) -> FullName {
    name.try_into().expect("valid name")
}

fn object(name: &str, hex: &str) -> Reference {
    Reference {
        name: full_name(name),
        target: Target::Object(id(hex)),
        peeled: None,
    }
}

fn symbolic(name: &str, target: &str) -> Reference {
    Reference {
        name: full_name(name),
        target: Target::Symbolic(full_name(target)),
        peeled: None,
    }
}

/// The references as `name target`, with `-> target` for symbolic ones and the
/// peeled object, if known, after `^`.
fn listing(refs: impl IntoIterator<Item = Reference>) -> Vec<String> {
    refs.into_iter()
        .map(|r| {
            let target = match &r.target {
                Target::Object(id) => id.to_string(),
                Target::Symbolic(name) => format!("-> {}", name.as_bstr()),
            };
            match r.peeled {
                Some(peeled) => format!("{} {target} ^{peeled}", r.name.as_bstr()),
                None => format!("{} {target}", r.name.as_bstr()),
            }
        })
        .collect()
}

fn all(store: &Store) -> Vec<String> {
    let platform = store.iter().expect("no packed-refs to open");
    listing(platform.all().expect("iterable").map(|r| r.expect("valid ref")))
}

fn prefixed(store: &Store, prefix: &str) -> Vec<String> {
    let platform = store.iter().expect("no packed-refs to open");
    let prefix: &gix_path::RelativePath = prefix.try_into().expect("valid prefix");
    listing(platform.prefixed(prefix).expect("iterable").map(|r| r.expect("valid ref")))
}

fn root_refs(store: &Store) -> Vec<String> {
    let platform = store.iter().expect("no packed-refs to open");
    listing(platform.pseudo().expect("iterable").map(|r| r.expect("valid ref")))
}

fn reflog_lines(store: &Store, name: &str) -> Option<Vec<String>> {
    let mut buf = Vec::new();
    let lines = store.reflog_iter(name, &mut buf).expect("readable")?;
    Some(
        lines
            .map(|line| {
                let line = line.expect("files-format line");
                let sig = line.signature;
                format!(
                    "{} {} {} <{}> {}\t{}",
                    line.previous_oid,
                    line.new_oid,
                    sig.name,
                    sig.email,
                    sig.time,
                    line.message
                )
            })
            .collect(),
    )
}

fn reflog_lines_rev(store: &Store, name: &str) -> Option<Vec<String>> {
    let mut buf = [0u8; 256];
    let lines = store.reflog_iter_rev(name, &mut buf).expect("readable")?;
    Some(
        lines
            .map(|line| {
                let line = line.expect("files-format line");
                format!("{} {} {}", line.previous_oid, line.new_oid, line.message)
            })
            .collect(),
    )
}

#[test]
fn single_references_come_from_the_stacks() {
    let Some(fx) = Fixture::new() else { return };
    let store = fx.main();
    assert_eq!(store.ref_storage(), RefStorage::Reftable);

    let find = |name: &str| store.try_find(name).expect("readable");
    assert_eq!(find("HEAD"), Some(symbolic("HEAD", "refs/heads/main")));
    assert_eq!(find("main"), Some(object("refs/heads/main", TWO)), "partial names expand as usual");
    assert_eq!(
        find("v1"),
        Some(object("refs/tags/v1", TAG)),
        "read_raw_ref() reports the tag, not its peeled value"
    );
    assert_eq!(find("refs/bisect/bad"), Some(object("refs/bisect/bad", TWO)));
    assert_eq!(
        find("refs/heads/gone"),
        None,
        "the tombstone in the newest table hides the branch in older ones"
    );
    assert_eq!(find("refs/worktree/x"), None, "the main worktree does not see wt's private refs");
    assert_eq!(
        find("worktrees/wt/HEAD"),
        Some(symbolic("worktrees/wt/HEAD", "refs/heads/side")),
        "another worktree's refs are read from its own stack"
    );
    assert_eq!(
        find("worktrees/wt/refs/worktree/x"),
        Some(object("worktrees/wt/refs/worktree/x", ONE))
    );
    assert!(store.open_packed_buffer().expect("no error").is_none());

    let wt = fx.worktree();
    let find = |name: &str| wt.try_find(name).expect("readable");
    assert_eq!(find("HEAD"), Some(symbolic("HEAD", "refs/heads/side")));
    assert_eq!(find("ORIG_HEAD"), Some(object("ORIG_HEAD", TWO)));
    assert_eq!(find("refs/worktree/x"), Some(object("refs/worktree/x", ONE)));
    assert_eq!(find("refs/heads/main"), Some(object("refs/heads/main", TWO)));
    assert_eq!(find("refs/bisect/bad"), None, "the main worktree's bisect refs are its own");
    assert_eq!(
        find("main-worktree/HEAD"),
        Some(symbolic("main-worktree/HEAD", "refs/heads/main"))
    );
}

#[test]
fn fetch_head_and_merge_head_stay_files() {
    let Some(fx) = Fixture::new() else { return };
    std::fs::write(
        fx.git_dir().join("FETCH_HEAD"),
        format!("{ONE}\t\tbranch 'main' of somewhere\n"),
    )
    .expect("writable");
    let store = fx.main();
    assert_eq!(
        store.try_find("FETCH_HEAD").expect("readable"),
        Some(object("FETCH_HEAD", ONE))
    );
    assert_eq!(store.try_find("MERGE_HEAD").expect("readable"), None);
}

#[test]
fn iteration_merges_the_worktree_stack_with_the_main_one() {
    let Some(fx) = Fixture::new() else { return };
    let store = fx.main();
    // `for-each-ref` without the root refs, in R.
    assert_eq!(
        all(&store),
        [
            format!("refs/bisect/bad {TWO}"),
            format!("refs/heads/main {TWO}"),
            format!("refs/heads/side {TWO}"),
            format!("refs/tags/v1 {TAG} ^{TWO}"),
        ]
    );
    assert_eq!(
        prefixed(&store, "refs/heads/"),
        [format!("refs/heads/main {TWO}"), format!("refs/heads/side {TWO}")]
    );
    assert_eq!(
        prefixed(&store, "refs/heads/s"),
        [format!("refs/heads/side {TWO}")],
        "a prefix need not end in a slash"
    );
    assert_eq!(root_refs(&store), ["HEAD -> refs/heads/main"]);

    // In wt: its own HEAD, ORIG_HEAD and refs/worktree/x, the shared refs of
    // the main stack, and not the main worktree's refs/bisect/bad.
    let wt = fx.worktree();
    assert_eq!(
        all(&wt),
        [
            format!("refs/heads/main {TWO}"),
            format!("refs/heads/side {TWO}"),
            format!("refs/tags/v1 {TAG} ^{TWO}"),
            format!("refs/worktree/x {ONE}"),
        ]
    );
    assert_eq!(root_refs(&wt), ["HEAD -> refs/heads/side".to_string(), format!("ORIG_HEAD {TWO}")]);
}

#[test]
fn reflogs_read_like_reflog_files() {
    let Some(fx) = Fixture::new() else { return };
    let store = fx.main();
    let committer = "C O Mitter <committer@example.com> 1112911993 -0700";

    // What `refs migrate --ref-format=files` writes into logs/HEAD and
    // logs/refs/heads/main for the same history.
    let expected = vec![
        format!("{NULL} {ONE} {committer}\tcommit (initial): one"),
        format!("{ONE} {TWO} {committer}\tcommit: two"),
    ];
    assert_eq!(reflog_lines(&store, "HEAD"), Some(expected.clone()));
    assert_eq!(reflog_lines(&store, "refs/heads/main"), Some(expected));
    assert_eq!(
        reflog_lines_rev(&store, "HEAD"),
        Some(vec![
            format!("{ONE} {TWO} commit: two"),
            format!("{NULL} {ONE} commit (initial): one"),
        ]),
        "the reverse iterator yields the newest entry first"
    );

    assert_eq!(
        reflog_lines(&store, "refs/heads/side"),
        Some(vec![]),
        "the existence marker keeps the reflog without showing up as an entry"
    );
    assert_eq!(reflog_lines(&store, "refs/heads/gone"), None);
    assert_eq!(reflog_lines(&store, "refs/tags/v1"), None);

    // `reflog exists`
    let exists = |store: &Store, name: &str| store.reflog_exists(name).expect("valid name");
    assert!(exists(&store, "HEAD"));
    assert!(exists(&store, "refs/heads/side"), "an existence marker is a reflog");
    assert!(!exists(&store, "refs/heads/gone"), "deleted with its branch");
    assert!(!exists(&store, "refs/tags/v1"));

    let wt = fx.worktree();
    // `git -C wt reflog`: HEAD@{0} 'reset: moving to HEAD', HEAD@{1} without message.
    assert_eq!(
        reflog_lines(&wt, "HEAD"),
        Some(vec![
            format!("{NULL} {TWO} {committer}\t"),
            format!("{TWO} {TWO} {committer}\treset: moving to HEAD"),
        ])
    );
    assert!(exists(&wt, "HEAD"));
    assert!(exists(&wt, "refs/heads/main"), "shared reflogs are read from the main stack");
}

#[test]
fn reflog_names_are_those_of_reflog_list() {
    let Some(fx) = Fixture::new() else { return };
    let names = |store: &Store| -> Vec<String> {
        store
            .reftable()
            .expect("a reftable store")
            .reflog_names()
            .expect("readable")
            .into_iter()
            .map(|name| name.as_bstr().to_string())
            .collect()
    };
    // `reflog list` in R and in wt: wt's own HEAD replaces the main
    // worktree's, `gone` is deleted and `side` still has its marker.
    assert_eq!(names(&fx.main()), ["HEAD", "refs/heads/main", "refs/heads/side"]);
    assert_eq!(names(&fx.worktree()), ["HEAD", "refs/heads/main", "refs/heads/side"]);
}

#[test]
fn exclude_patterns_and_per_worktree_only() {
    let Some(fx) = Fixture::new() else { return };
    let store = fx.main();
    let backend = store.reftable().expect("a reftable store");
    let names = |iter: gix_ref::reftable::RefIter| -> Vec<String> {
        iter.map(|r| r.expect("valid ref").name.as_bstr().to_string()).collect()
    };
    // `for-each-ref --exclude=refs/heads/ --exclude=refs/tags/*`: the glob
    // pattern cannot be skipped by seeking and is left to the caller.
    assert_eq!(
        names(backend.iter_refs("".into(), &["refs/heads/".into(), "refs/tags/*".into()], 0)),
        ["refs/bisect/bad", "refs/tags/v1"]
    );
    assert_eq!(
        names(backend.iter_refs(
            "".into(),
            &[],
            gix_ref::reftable::RefIter::PER_WORKTREE_ONLY | gix_ref::reftable::RefIter::INCLUDE_ROOT_REFS
        )),
        ["HEAD", "refs/bisect/bad"]
    );
}
