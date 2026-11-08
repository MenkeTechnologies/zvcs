//! The refs a clone writes, written in the transactions `cmd_clone()` makes.
//!
//! `update_remote_refs()` creates every mapped ref in one `REF_TRANSACTION_FLAG_INITIAL`
//! transaction (`write_remote_refs()`, builtin/clone.c:473-498): the files backend puts
//! them straight into `packed-refs` without reflogs, and the `reference-transaction`
//! hook sees `preparing` and `prepared` for it and for its nested `packed-refs`
//! transaction, with no `committed` (refs.c:2780-2786, refs/files-backend.c:3194-3300).
//! The tags a `--single-branch` clone follows, `refs/remotes/<name>/HEAD` and `HEAD`
//! are then written one transaction each (builtin/clone.c:500-613), loose and logged.
//!
//! Each case clones the same sources with both binaries, the hook installed through
//! `--template`, and compares the output, the hook's log, `packed-refs`, the loose refs,
//! the reflogs, the config and what stock's `for-each-ref`, `reflog` and `fsck` read back.
//!
//! `--no-tags --branch` still maps every tag: `TAG_REFSPEC` is appended for `--branch`
//! as well (builtin/clone.c:1414-1419).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

const HOOK: &str = "#!/bin/sh\n\
    printf '== %s\\n' \"$1\" >>\"$HOOKLOG\"\n\
    cat >>\"$HOOKLOG\"\n\
    exit 0\n";

fn git(bin: &str, cwd: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("HOOKLOG", home.join("hook.log"))
        .stdin(Stdio::null())
        .output()
        .expect("git runs")
}

fn ok(bin: &str, cwd: &Path, home: &Path, args: &[&str]) {
    let out = git(bin, cwd, home, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// The sources every case clones: `src` with two branches, a lightweight and an annotated
/// tag on the older commit and tags on each tip; `det`, the same with a detached `HEAD`;
/// `empty`, a bare repository without commits; `src.bundle`, all of `src`.
fn sources(root: &Path, stock: &str) {
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let run = |args: &[&str]| ok(stock, &src, root, args);
    run(&["init", "-q", "-b", "main"]);
    std::fs::write(src.join("a"), "a\n").unwrap();
    run(&["add", "a"]);
    run(&["-c", "maintenance.auto=false", "commit", "-qm", "one"]);
    run(&["tag", "v1"]);
    run(&["tag", "-a", "-m", "annotated", "v2"]);
    run(&["branch", "feature"]);
    std::fs::write(src.join("b"), "b\n").unwrap();
    run(&["add", "b"]);
    run(&["-c", "maintenance.auto=false", "commit", "-qm", "two"]);
    run(&["tag", "v3"]);
    run(&["checkout", "-q", "feature"]);
    std::fs::write(src.join("c"), "c\n").unwrap();
    run(&["add", "c"]);
    run(&["-c", "maintenance.auto=false", "commit", "-qm", "three"]);
    run(&["tag", "ftag"]);
    run(&["checkout", "-q", "main"]);
    run(&["bundle", "create", "-q", "../src.bundle", "--all"]);

    ok(stock, root, root, &["clone", "-q", "--no-local", "src", "det"]);
    ok(stock, &root.join("det"), root, &["checkout", "-q", "--detach", "HEAD~1"]);
    ok(stock, root, root, &["init", "-q", "--bare", "empty"]);

    let hooks = root.join("template").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("reference-transaction");
    std::fs::write(&hook, HOOK).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Every file below `dir`, relative and sorted, with its content; directories as names.
fn tree(dir: &Path) -> String {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                out.push(format!("{rel}/"));
                walk(base, &path, out);
            } else {
                out.push(format!("{rel}: {}", std::fs::read_to_string(&path).unwrap_or_default()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out.join("\n")
}

/// Clone `source` with `bin` and `args` into a fresh directory below `root`, and report
/// everything the comparison covers.
fn clone(bin: &str, stock: &str, root: &Path, side: &str, source: &str, args: &[&str]) -> String {
    let home = root.join(side);
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let template = format!("--template={}", root.join("template").display());
    let mut argv = vec!["clone", template.as_str()];
    argv.extend_from_slice(args);
    argv.push(source);
    argv.push("dst");
    let out = git(bin, &home, &home, &argv);
    let dst = home.join("dst");
    let git_dir = if dst.join(".git").is_dir() { dst.join(".git") } else { dst.clone() };
    // A clone that died leaves no repository to read.
    let read = |args: &[&str]| {
        if !git_dir.is_dir() {
            return String::new();
        }
        let out = git(stock, &git_dir, &home, args);
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    };
    let mut reflogs = String::new();
    for name in read(&["reflog", "list"]).lines() {
        reflogs.push_str(&read(&["log", "-g", "--format=%gD %H %gs", name]));
    }
    let report = format!(
        "exit={}\n--- stdout\n{}--- stderr\n{}--- hook\n{}--- HEAD\n{}--- packed-refs\n{}--- refs\n{}\n--- logs\n{}\n--- for-each-ref\n{}--- reflogs\n{}--- fsck\n{}--- config\n{}",
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        std::fs::read_to_string(home.join("hook.log")).unwrap_or_default(),
        std::fs::read_to_string(git_dir.join("HEAD")).unwrap_or_default(),
        std::fs::read_to_string(git_dir.join("packed-refs")).unwrap_or_default(),
        tree(&git_dir.join("refs")),
        tree(&git_dir.join("logs")),
        read(&["for-each-ref", "--format=%(refname) %(objectname) %(symref)"]),
        reflogs,
        read(&["fsck", "--no-progress"]),
        std::fs::read_to_string(git_dir.join("config")).unwrap_or_default(),
    );
    report.replace(&home.to_string_lossy().into_owned(), "<home>")
}

/// The option sets every source is cloned with.
const OPTIONS: &[&[&str]] = &[
    &[],
    &["--bare"],
    &["--mirror"],
    &["--single-branch"],
    &["--bare", "--single-branch"],
    &["--no-tags"],
    &["--no-tags", "-b", "feature"],
    &["--no-tags", "-b", "feature", "--bare"],
    &["-b", "feature"],
    &["-b", "v2"],
    &["--depth", "1"],
    &["--revision", "refs/heads/feature"],
    &["--revision", "refs/tags/v2"],
    &["--no-checkout"],
    &["-o", "up"],
    &["--ref-format", "reftable"],
    &["--ref-format", "reftable", "--bare"],
    &["--ref-format", "reftable", "--single-branch"],
    &["-c", "core.logAllRefUpdates=true", "--bare"],
    &["-c", "core.logAllRefUpdates=always", "--single-branch"],
];

/// Clone `src`, `empty` and `det` as `spell` names them with [`OPTIONS`] (only `src`) and
/// `extra`, through both binaries, and compare.
fn compare(tag: &str, spell: fn(&Path) -> String, extra: &[(&str, &[&str])]) {
    let Some(stock) = stock_git::stock_git() else { return };
    let root: PathBuf =
        std::env::temp_dir().join(format!("zvcs-clone-initial-transaction-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    sources(&root, stock);

    let mut cases: Vec<(String, Vec<&str>)> =
        OPTIONS.iter().map(|args| (spell(&root.join("src")), args.to_vec())).collect();
    for name in ["empty", "det"] {
        cases.push((spell(&root.join(name)), Vec::new()));
        cases.push((spell(&root.join(name)), vec!["--bare"]));
    }
    for (name, args) in extra {
        cases.push((spell(&root.join(name)), args.to_vec()));
    }

    let mut differing = Vec::new();
    for (source, args) in &cases {
        let want = clone(stock, stock, &root, "stock", source, args);
        let got = clone(ZVCS, stock, &root, "zvcs", source, args);
        if got != want {
            let source = source.replace(&root.to_string_lossy().into_owned(), "<root>");
            differing.push(format!("clone {args:?} {source}\n--- stock\n{want}\n--- zvcs\n{got}"));
        }
    }
    assert!(differing.is_empty(), "{} of {} clones differ:\n{}", differing.len(), cases.len(), differing.join("\n"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_local_clone_writes_its_refs_in_the_transactions_stock_makes() {
    compare("path", |path| path.to_string_lossy().into_owned(), &[
        ("src.bundle", &[]),
        ("src.bundle", &["--bare"]),
        // A tag a `--single-branch` clone does not reach is not followed. Over `file://` the
        // transport sends more than stock's `include-tag` would (see the deviations of
        // `porcelain/clone.rs`), so only the local path, which copies every object, has it.
        ("src", &["-b", "v2", "--single-branch"]),
        // `--mirror --branch` maps every tag twice, which `ref_update_reject_duplicates()` refuses.
        ("src", &["--mirror", "-b", "feature"]),
    ]);
}

#[test]
fn a_file_url_clone_writes_its_refs_in_the_transactions_stock_makes() {
    compare("url", |path| format!("file://{}", path.display()), &[
        // A partial clone's followed tags are those whose objects the pack brought: the
        // promisor remote is not asked for the rest (`odb_has_object()` without
        // `HAS_OBJECT_FETCH_PROMISOR`, builtin/clone.c:508).
        ("src", &["--filter=blob:none", "--single-branch"]),
    ]);
}
