//! Transactions on a reftable store, checked against stock git.
//!
//! Each test builds the repository with stock git, copies it, and applies the
//! same changes twice: with stock git to one copy and through
//! `file::Store::transaction()` to the other. Stock git then has to see the same
//! references, the same reflogs (`for-each-ref --include-root-refs`, `reflog
//! list`, `log -g --date=raw`) and the same verdict of `refs verify` and `fsck`
//! in both. Committer and dates are fixed, so the reflogs compare byte by byte.

#[path = "../../../extensions/tests/support/stock_git.rs"]
mod stock_git;

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use gix_lock::acquire::Fail;
use gix_ref::{
    FullName, Target,
    file::{
        Store,
        transaction::{PackedRefs, prepare},
    },
    store::{RefStorage, WriteReflog},
    transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
};

/// The committer of every reflog entry, on both sides.
const NAME: &str = "C O Mitter";
const EMAIL: &str = "committer@example.com";
const DATE: &str = "1600000000 +0100";

/// A temporary directory holding the stock-built repository `R`.
struct Fixture {
    _tmp: gix_testtools::tempfile::TempDir,
    root: PathBuf,
    git: &'static str,
}

impl Fixture {
    /// `R` of the plan: two empty commits on `main`, a branch `side`, and an
    /// annotated tag `v1`, with `--ref-format=reftable`. With `worktree`, also
    /// a linked worktree `wt` on `side`. `None` without a stock git.
    fn new(worktree: bool) -> Option<Fixture> {
        let Some(git) = stock_git::stock_git_at_least((2, 56, 0)) else {
            eprintln!("skipped: no stock git 2.56");
            return None;
        };
        let tmp = gix_testtools::tempfile::tempdir().expect("temp dir");
        let root = tmp.path().to_owned();
        let fixture = Fixture { _tmp: tmp, root, git };
        let r = fixture.root.join("R");
        fixture.ok(
            &fixture.root,
            &["init", "-q", "-b", "main", "--ref-format=reftable", "R"],
        );
        fixture.ok(&r, &["commit", "-q", "--allow-empty", "-m", "one"]);
        fixture.ok(&r, &["commit", "-q", "--allow-empty", "-m", "two"]);
        fixture.ok(&r, &["branch", "side"]);
        fixture.ok(&r, &["tag", "-a", "v1", "-m", "t"]);
        if worktree {
            fixture.ok(&r, &["worktree", "add", "-q", "../wt", "side"]);
        }
        Some(fixture)
    }

    /// Run stock git in `dir`, hermetically.
    fn run(&self, dir: &Path, args: &[&str]) -> std::process::Output {
        self.run_with_input(dir, args, b"")
    }

    /// Run stock git in `dir`, hermetically, with `input` on its stdin.
    fn run_with_input(&self, dir: &Path, args: &[&str], input: &[u8]) -> std::process::Output {
        use std::io::Write;
        let mut child = Command::new(self.git)
            .args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", NAME)
            .env("GIT_AUTHOR_EMAIL", EMAIL)
            .env("GIT_AUTHOR_DATE", DATE)
            .env("GIT_COMMITTER_NAME", NAME)
            .env("GIT_COMMITTER_EMAIL", EMAIL)
            .env("GIT_COMMITTER_DATE", DATE)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("stock git runs");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input)
            .expect("write stdin");
        child.wait_with_output().expect("stock git finishes")
    }

    /// Run stock git in `dir` and return its stdout, panicking on failure.
    fn ok(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.run(dir, args);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf8")
    }

    /// The message stock git dies with, without its `fatal: ` prefix and the
    /// `update_ref failed for ref '…': ` that `update-ref` adds.
    fn fails(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.run(dir, args);
        assert!(!out.status.success(), "git {args:?} was expected to fail");
        let err = String::from_utf8(out.stderr).expect("utf8");
        let err = err
            .trim_end()
            .trim_start_matches("fatal: ")
            .trim_start_matches("error: ");
        match err.strip_prefix("update_ref failed for ref '") {
            Some(rest) => rest.split_once("': ").expect("refname is quoted").1.to_owned(),
            None => err.to_owned(),
        }
    }

    /// A copy of the directory `name` under the fixture root, as `to`.
    fn copy(&self, name: &str, to: &str) -> PathBuf {
        fn copy_dir(from: &Path, to: &Path) {
            std::fs::create_dir_all(to).expect("create dir");
            for entry in std::fs::read_dir(from).expect("read dir") {
                let entry = entry.expect("dir entry");
                let target = to.join(entry.file_name());
                if entry.file_type().expect("file type").is_dir() {
                    copy_dir(&entry.path(), &target);
                } else {
                    std::fs::copy(entry.path(), &target).expect("copy file");
                }
            }
        }
        let to = self.root.join(to);
        copy_dir(&self.root.join(name), &to);
        to
    }

    fn rev(&self, dir: &Path, spec: &str) -> gix_hash::ObjectId {
        gix_hash::ObjectId::from_hex(self.ok(dir, &["rev-parse", spec]).trim().as_bytes()).expect("hex id")
    }

    /// What stock git reports about the references and reflogs of the
    /// repository whose worktree is `dir`.
    fn observe(&self, dir: &Path) -> String {
        let mut out = self.ok(dir, &["for-each-ref", "--include-root-refs"]);
        let logs = self.ok(dir, &["reflog", "list"]);
        out.push_str(&logs);
        for name in logs.lines() {
            out.push_str(&self.ok(dir, &["log", "-g", "--date=raw", name]));
        }
        out
    }

    /// Stock git's verdict on the repository whose worktree is `dir`.
    fn verdict(&self, dir: &Path) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        for args in [&["refs", "verify"][..], &["fsck"][..]] {
            let res = self.run(dir, args);
            write!(
                out,
                "{args:?}: {} {}{}",
                res.status,
                String::from_utf8_lossy(&res.stdout),
                String::from_utf8_lossy(&res.stderr)
            )
            .expect("writing to a String");
        }
        out
    }

    /// Both copies look the same to stock git, and stock git finds nothing
    /// wrong with either.
    fn assert_same(&self, ours: &Path, stock: &Path) {
        assert_eq!(
            self.observe(ours),
            self.observe(stock),
            "references and reflogs match stock"
        );
        let verdict = self.verdict(ours);
        assert_eq!(verdict, self.verdict(stock), "stock verification agrees");
        assert!(
            verdict.matches("exit status: 0").count() == 2,
            "refs verify and fsck pass: {verdict}"
        );
    }
}

fn options() -> gix_ref::store::init::Options {
    gix_ref::store::init::Options {
        write_reflog: WriteReflog::Normal,
        object_hash: gix_hash::Kind::Sha1,
        ref_storage: RefStorage::Reftable,
        ..Default::default()
    }
}

/// The reftable store of the main worktree at `dir`.
fn store(dir: &Path) -> Store {
    Store::at(dir.join(".git"), options())
}

fn committer() -> gix_actor::SignatureRef<'static> {
    gix_actor::SignatureRef {
        name: NAME.into(),
        email: EMAIL.into(),
        time: DATE,
    }
}

fn name(name: &str) -> FullName {
    name.try_into().expect("valid name")
}

fn update(refname: &str, new: Target, expected: PreviousValue, message: &str, deref: bool) -> RefEdit {
    RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.into(),
            },
            expected,
            new,
        },
        name: name(refname),
        deref,
    }
}

fn delete(refname: &str, expected: PreviousValue, deref: bool) -> RefEdit {
    RefEdit {
        change: Change::Delete {
            expected,
            log: RefLog::AndReference,
            message: Default::default(),
        },
        name: name(refname),
        deref,
    }
}

/// Prepare and commit `edits` on `store`, peeling tags with the objects of
/// the repository at `dir`.
fn apply(store: &Store, dir: &Path, edits: Vec<RefEdit>) -> Result<Vec<RefEdit>, String> {
    let objects = gix_odb::at(dir.join(".git/objects")).expect("object database");
    store
        .transaction()
        .packed_refs(PackedRefs::DeletionsAndNonSymbolicUpdates(Box::new(objects)))
        .prepare(edits, Fail::Immediately, Fail::Immediately)
        .map_err(|err| err.to_string())?
        .commit(committer())
        .map_err(|err| err.to_string())
}

fn tables(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join(".git/reftable/tables.list"))
        .expect("tables.list")
        .lines()
        .count()
}

/// Updates, creations, deletions, symbolic and detached `HEAD`, and a tag
/// whose peeled value goes into the table.
#[test]
fn updates_match_stock() {
    let Some(f) = Fixture::new(false) else { return };
    let (ours, stock) = (f.copy("R", "ours"), f.copy("R", "stock"));
    let one = f.rev(&stock, "HEAD~");
    let two = f.rev(&stock, "HEAD");
    let tag = f.rev(&stock, "v1");
    let (one_hex, two_hex, tag_hex) = (one.to_string(), two.to_string(), tag.to_string());
    let zero = gix_hash::Kind::Sha1.null().to_string();
    let store = store(&ours);

    // The checked-out branch: its log and HEAD's gain an entry.
    f.ok(
        &stock,
        &["update-ref", "-m", "first  move\n", "refs/heads/main", &one_hex],
    );
    apply(
        &store,
        &ours,
        vec![update(
            "refs/heads/main",
            Target::Object(one),
            PreviousValue::Any,
            "first  move\n",
            true,
        )],
    )
    .expect("update main");

    // A creation without a message.
    f.ok(&stock, &["update-ref", "refs/heads/new", &two_hex, &zero]);
    apply(
        &store,
        &ours,
        vec![update(
            "refs/heads/new",
            Target::Object(two),
            PreviousValue::MustNotExist,
            "",
            true,
        )],
    )
    .expect("create new");

    // Through HEAD, which is split into a log-only HEAD and the branch.
    f.ok(&stock, &["update-ref", "-m", "via head", "HEAD", &two_hex, &one_hex]);
    let edits = apply(
        &store,
        &ours,
        vec![update(
            "HEAD",
            Target::Object(two),
            PreviousValue::MustExistAndMatch(Target::Object(one)),
            "via head",
            true,
        )],
    )
    .expect("update through HEAD");
    assert_eq!(
        edits.iter().map(|e| e.name.as_bstr().to_string()).collect::<Vec<_>>(),
        ["HEAD", "refs/heads/main"],
        "the split shows in the edits"
    );

    // Detach, then attach again.
    f.ok(&stock, &["update-ref", "--no-deref", "-m", "detach", "HEAD", &one_hex]);
    apply(
        &store,
        &ours,
        vec![update("HEAD", Target::Object(one), PreviousValue::Any, "detach", false)],
    )
    .expect("detach");
    f.ok(&stock, &["symbolic-ref", "-m", "attach", "HEAD", "refs/heads/main"]);
    apply(
        &store,
        &ours,
        vec![update(
            "HEAD",
            Target::Symbolic(name("refs/heads/main")),
            PreviousValue::Any,
            "attach",
            false,
        )],
    )
    .expect("attach");

    // Deletions take their reflog with them.
    f.ok(&stock, &["update-ref", "-d", "refs/heads/side"]);
    apply(&store, &ours, vec![delete("refs/heads/side", PreviousValue::Any, true)]).expect("delete side");
    f.ok(&stock, &["update-ref", "-d", "refs/heads/new", &two_hex]);
    apply(
        &store,
        &ours,
        vec![delete(
            "refs/heads/new",
            PreviousValue::MustExistAndMatch(Target::Object(two)),
            true,
        )],
    )
    .expect("delete new");

    // A tag object: the table stores its peeled value, which `show-ref -d` reports.
    f.ok(&stock, &["update-ref", "refs/tags/v2", &tag_hex]);
    apply(
        &store,
        &ours,
        vec![update(
            "refs/tags/v2",
            Target::Object(tag),
            PreviousValue::Any,
            "",
            true,
        )],
    )
    .expect("create tag");

    // Several references in one transaction, in one table.
    let before = tables(&ours);
    let stdin = format!("update refs/heads/a {one_hex}\nupdate refs/heads/b {two_hex}\n");
    let out = f.run_with_input(&stock, &["update-ref", "-m", "both", "--stdin"], stdin.as_bytes());
    assert!(out.status.success(), "update-ref --stdin");
    apply(
        &store,
        &ours,
        vec![
            update("refs/heads/a", Target::Object(one), PreviousValue::Any, "both", true),
            update("refs/heads/b", Target::Object(two), PreviousValue::Any, "both", true),
        ],
    )
    .expect("two at once");
    assert!(tables(&ours) <= before + 1, "one transaction adds at most one table");

    f.assert_same(&ours, &stock);
    assert_eq!(
        f.ok(&ours, &["show-ref", "-d", "v2"]),
        f.ok(&stock, &["show-ref", "-d", "v2"]),
        "the peeled value is there"
    );
}

/// Refused transactions carry stock git's message and change nothing.
#[test]
fn refusals_match_stock() {
    let Some(f) = Fixture::new(false) else { return };
    let (ours, stock) = (f.copy("R", "ours"), f.copy("R", "stock"));
    let one = f.rev(&stock, "HEAD~");
    let two = f.rev(&stock, "HEAD");
    let (one_hex, two_hex) = (one.to_string(), two.to_string());
    let zero = gix_hash::Kind::Sha1.null().to_string();
    let store = store(&ours);
    let refused = |edits: Vec<RefEdit>| apply(&store, &ours, edits).expect_err("refused");

    assert_eq!(
        refused(vec![update(
            "refs/heads/main",
            Target::Object(one),
            PreviousValue::MustExistAndMatch(Target::Object(one)),
            "",
            true
        )]),
        f.fails(&stock, &["update-ref", "refs/heads/main", &one_hex, &one_hex]),
        "wrong old value"
    );
    assert_eq!(
        refused(vec![update(
            "refs/heads/side",
            Target::Object(one),
            PreviousValue::MustNotExist,
            "",
            true
        )]),
        f.fails(&stock, &["update-ref", "refs/heads/side", &one_hex, &zero]),
        "creating what exists"
    );
    assert_eq!(
        refused(vec![update(
            "refs/heads/nope",
            Target::Object(one),
            PreviousValue::MustExistAndMatch(Target::Object(two)),
            "",
            true
        )]),
        f.fails(&stock, &["update-ref", "refs/heads/nope", &one_hex, &two_hex]),
        "updating what does not exist"
    );
    assert_eq!(
        refused(vec![delete(
            "refs/heads/gone",
            PreviousValue::MustExistAndMatch(Target::Object(one)),
            true
        )]),
        f.fails(&stock, &["update-ref", "-d", "refs/heads/gone", &one_hex]),
        "deleting what does not exist"
    );
    assert_eq!(
        refused(vec![update(
            "refs/heads/main/x",
            Target::Object(one),
            PreviousValue::Any,
            "",
            true
        )]),
        f.fails(&stock, &["update-ref", "refs/heads/main/x", &one_hex]),
        "a reference as directory"
    );
    assert_eq!(
        refused(vec![update(
            "refs/heads",
            Target::Object(one),
            PreviousValue::Any,
            "",
            true
        )]),
        f.fails(&stock, &["update-ref", "refs/heads", &one_hex]),
        "a directory of references"
    );
    assert_eq!(
        refused(vec![update(
            "FETCH_HEAD",
            Target::Object(one),
            PreviousValue::Any,
            "",
            true
        )]),
        f.fails(&stock, &["update-ref", "FETCH_HEAD", &one_hex]),
        "pseudo references stay files"
    );

    // The stack is locked by someone else.
    let lock = ours.join(".git/reftable/tables.list.lock");
    std::fs::write(&lock, "").expect("lock");
    std::fs::write(stock.join(".git/reftable/tables.list.lock"), "").expect("lock");
    assert_eq!(
        refused(vec![update(
            "refs/heads/z",
            Target::Object(one),
            PreviousValue::Any,
            "",
            true
        )]),
        f.fails(&stock, &["update-ref", "refs/heads/z", &one_hex]),
        "a locked stack"
    );
    std::fs::remove_file(&lock).expect("unlock");
    std::fs::remove_file(stock.join(".git/reftable/tables.list.lock")).expect("unlock");

    let err = store
        .transaction()
        .prepare(
            vec![
                update("refs/heads/q", Target::Object(one), PreviousValue::Any, "", true),
                update("refs/heads/q/r", Target::Object(one), PreviousValue::Any, "", true),
            ],
            Fail::Immediately,
            Fail::Immediately,
        )
        .expect_err("conflicting names in one transaction");
    assert!(matches!(
        err,
        prepare::Error::Reftable {
            kind: gix_ref::file::transaction::ErrorKind::NameConflict,
            ..
        }
    ));
    assert_eq!(
        err.to_string(),
        "cannot process 'refs/heads/q' and 'refs/heads/q/r' at the same time"
    );

    f.assert_same(&ours, &stock);
}

/// A prepared transaction that is dropped releases its lock and writes nothing.
#[test]
fn dropping_a_prepared_transaction_aborts_it() {
    let Some(f) = Fixture::new(false) else { return };
    let (ours, stock) = (f.copy("R", "ours"), f.copy("R", "stock"));
    let one = f.rev(&ours, "HEAD~");
    let store = store(&ours);
    let list = std::fs::read(ours.join(".git/reftable/tables.list")).expect("tables.list");

    let prepared = store
        .transaction()
        .prepare(
            vec![update(
                "refs/heads/main",
                Target::Object(one),
                PreviousValue::Any,
                "",
                true,
            )],
            Fail::Immediately,
            Fail::Immediately,
        )
        .expect("prepared");
    assert!(
        ours.join(".git/reftable/tables.list.lock").is_file(),
        "a prepared transaction holds the stack's lock"
    );
    let edits = prepared.rollback();
    assert_eq!(edits.len(), 2, "the edit and the HEAD log entry it implies");

    assert!(
        !ours.join(".git/reftable/tables.list.lock").exists(),
        "the lock is released"
    );
    assert_eq!(
        std::fs::read(ours.join(".git/reftable/tables.list")).expect("tables.list"),
        list,
        "nothing was written"
    );
    f.assert_same(&ours, &stock);
}

/// Twenty transactions leave as many tables as they leave with stock git:
/// each commit auto-compacts the stack the same way.
#[test]
fn auto_compaction_matches_stock() {
    let Some(f) = Fixture::new(false) else { return };
    let (ours, stock) = (f.copy("R", "ours"), f.copy("R", "stock"));
    let ids = [f.rev(&stock, "HEAD~"), f.rev(&stock, "HEAD")];
    let store = store(&ours);
    for round in 0..20 {
        let id = ids[round % 2];
        let message = format!("round {round}");
        f.ok(
            &stock,
            &["update-ref", "-m", &message, "refs/heads/side", &id.to_string()],
        );
        apply(
            &store,
            &ours,
            vec![update(
                "refs/heads/side",
                Target::Object(id),
                PreviousValue::Any,
                &message,
                true,
            )],
        )
        .expect("update");
        assert_eq!(tables(&ours), tables(&stock), "tables after round {round}");
    }
    f.assert_same(&ours, &stock);
}

/// In a linked worktree, `HEAD` lives in the worktree's own stack: updating
/// the branch it points to writes the branch to the main stack and `HEAD`'s
/// log entry to the worktree's.
#[test]
fn worktree_head_goes_to_the_worktree_stack() {
    let Some(f) = Fixture::new(true) else { return };
    let ours = f.copy("R", "ours");
    let ours_wt = f.copy("wt", "ours-wt");
    let stock = f.copy("R", "stock");
    let stock_wt = f.copy("wt", "stock-wt");
    // Each copy of the worktree points at its own copy of the repository.
    for (repo, wt) in [(&ours, &ours_wt), (&stock, &stock_wt)] {
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", repo.join(".git/worktrees/wt").display()),
        )
        .expect("worktree link");
        std::fs::write(
            repo.join(".git/worktrees/wt/gitdir"),
            format!("{}\n", wt.join(".git").display()),
        )
        .expect("worktree back link");
    }
    let one = f.rev(&stock, "HEAD~");

    f.ok(
        &stock_wt,
        &["update-ref", "-m", "in wt", "refs/heads/side", &one.to_string()],
    );
    let store = Store::for_linked_worktree(ours.join(".git/worktrees/wt"), ours.join(".git"), options());
    apply(
        &store,
        &ours,
        vec![update(
            "refs/heads/side",
            Target::Object(one),
            PreviousValue::Any,
            "in wt",
            true,
        )],
    )
    .expect("update side from the worktree");

    f.assert_same(&ours, &stock);
    assert_eq!(
        f.observe(&ours_wt),
        f.observe(&stock_wt),
        "the worktree's view matches stock"
    );
    assert_eq!(
        f.ok(&ours_wt, &["log", "-g", "--date=raw", "-1", "--format=%gs", "HEAD"]),
        "in wt\n",
        "HEAD of the worktree logged the update"
    );
}

/// Symbolic updates with an expected old target (`symref-update`, `symref-verify`),
/// a dangling symbolic `HEAD`, which gets no log entry, and deleting the
/// checked-out branch, which leaves `HEAD` a log entry to the null id.
#[test]
fn symbolic_updates_match_stock() {
    let Some(f) = Fixture::new(false) else { return };
    let (ours, stock) = (f.copy("R", "ours"), f.copy("R", "stock"));
    let store = store(&ours);

    // Dereferenced, the expectation applies to `main`, which is no symref.
    let symref_update = b"symref-update HEAD refs/heads/side ref refs/heads/main\n";
    let out = f.run_with_input(&stock, &["update-ref", "-m", "switch", "--stdin"], symref_update);
    let ours_err = apply(
        &store,
        &ours,
        vec![update(
            "HEAD",
            Target::Symbolic(name("refs/heads/side")),
            PreviousValue::MustExistAndMatch(Target::Symbolic(name("refs/heads/main"))),
            "switch",
            true,
        )],
    )
    .expect_err("main is a regular ref");
    assert_eq!(
        String::from_utf8(out.stderr).expect("utf8"),
        format!("fatal: {ours_err}\n"),
        "the split-off update checks the old target"
    );

    let out = f.run_with_input(
        &stock,
        &["update-ref", "--no-deref", "-m", "switch", "--stdin"],
        symref_update,
    );
    assert!(out.status.success(), "symref-update");
    apply(
        &store,
        &ours,
        vec![update(
            "HEAD",
            Target::Symbolic(name("refs/heads/side")),
            PreviousValue::MustExistAndMatch(Target::Symbolic(name("refs/heads/main"))),
            "switch",
            false,
        )],
    )
    .expect("symref-update");

    let out = f.run_with_input(
        &stock,
        &["update-ref", "--no-deref", "--stdin"],
        b"symref-verify HEAD refs/heads/main\n",
    );
    let stock_err = String::from_utf8(out.stderr).expect("utf8");
    let ours_err = apply(
        &store,
        &ours,
        vec![update(
            "HEAD",
            Target::Symbolic(name("refs/heads/side")),
            PreviousValue::MustExistAndMatch(Target::Symbolic(name("refs/heads/main"))),
            "",
            false,
        )],
    )
    .expect_err("HEAD points elsewhere");
    assert_eq!(stock_err, format!("fatal: {ours_err}\n"), "a wrong old target");

    // `side` is checked out now: deleting it logs `<old> <null>` for HEAD.
    f.ok(&stock, &["update-ref", "-m", "drop", "-d", "refs/heads/side"]);
    let mut edit = delete("refs/heads/side", PreviousValue::Any, true);
    if let Change::Delete { message, .. } = &mut edit.change {
        *message = "drop".into();
    }
    apply(&store, &ours, vec![edit]).expect("delete the checked-out branch");

    // Pointing HEAD at a branch yet to be born writes no log entry.
    f.ok(&stock, &["symbolic-ref", "-m", "unborn", "HEAD", "refs/heads/unborn"]);
    apply(
        &store,
        &ours,
        vec![update(
            "HEAD",
            Target::Symbolic(name("refs/heads/unborn")),
            PreviousValue::Any,
            "unborn",
            false,
        )],
    )
    .expect("dangling HEAD");
    f.ok(&stock, &["symbolic-ref", "-m", "back", "HEAD", "refs/heads/main"]);
    apply(
        &store,
        &ours,
        vec![update(
            "HEAD",
            Target::Symbolic(name("refs/heads/main")),
            PreviousValue::Any,
            "back",
            false,
        )],
    )
    .expect("HEAD back on main");

    f.assert_same(&ours, &stock);
    assert_eq!(
        f.ok(&ours, &["rev-parse", "main"]).trim(),
        f.rev(&stock, "main").to_string()
    );
}

/// `core.logAllRefUpdates=false` logs only references that have a log
/// already, `--create-reflog` forces one, and a message is cut to half the
/// block size.
#[test]
fn reflog_decisions_match_stock() {
    let Some(f) = Fixture::new(false) else { return };
    let (ours, stock) = (f.copy("R", "ours"), f.copy("R", "stock"));
    let one = f.rev(&stock, "HEAD~");
    let one_hex = one.to_string();
    let store = store(&ours);
    store
        .reftable()
        .expect("a reftable store")
        .set_write_config_fn(std::sync::Arc::new(|| gix_ref::reftable::WriteConfig {
            log_all_ref_updates: Some(WriteReflog::Disable),
            ..Default::default()
        }));
    let no_logs = ["-c", "core.logAllRefUpdates=false"];

    // `side` has a log, `fresh` gets none, `refs/tags/forced` is forced to.
    f.ok(
        &stock,
        &[&no_logs[..], &["update-ref", "-m", "kept", "refs/heads/side", &one_hex]].concat(),
    );
    f.ok(
        &stock,
        &[
            &no_logs[..],
            &["update-ref", "-m", "none", "refs/heads/fresh", &one_hex],
        ]
        .concat(),
    );
    f.ok(
        &stock,
        &[
            &no_logs[..],
            &[
                "update-ref",
                "--create-reflog",
                "-m",
                "forced",
                "refs/tags/forced",
                &one_hex,
            ],
        ]
        .concat(),
    );
    let long = "x".repeat(3000);
    f.ok(
        &stock,
        &[
            &no_logs[..],
            &[
                "update-ref",
                "-m",
                &long,
                "refs/heads/side",
                &f.rev(&stock, "HEAD").to_string(),
            ],
        ]
        .concat(),
    );

    apply(
        &store,
        &ours,
        vec![update(
            "refs/heads/side",
            Target::Object(one),
            PreviousValue::Any,
            "kept",
            true,
        )],
    )
    .expect("update side");
    apply(
        &store,
        &ours,
        vec![update(
            "refs/heads/fresh",
            Target::Object(one),
            PreviousValue::Any,
            "none",
            true,
        )],
    )
    .expect("create fresh");
    let mut forced = update(
        "refs/tags/forced",
        Target::Object(one),
        PreviousValue::Any,
        "forced",
        true,
    );
    if let Change::Update { log, .. } = &mut forced.change {
        log.force_create_reflog = true;
    }
    apply(&store, &ours, vec![forced]).expect("forced log");
    apply(
        &store,
        &ours,
        vec![update(
            "refs/heads/side",
            Target::Object(f.rev(&stock, "HEAD")),
            PreviousValue::Any,
            &long,
            true,
        )],
    )
    .expect("long message");

    f.assert_same(&ours, &stock);
    let message = f.ok(&ours, &["log", "-g", "-1", "--format=%gs", "side"]);
    assert_eq!(message.trim_end().len(), 2048, "half of the 4096 byte block size");
}
