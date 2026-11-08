//! `receive-pack`'s connectivity check and aliased-update check, against stock
//! git as the oracle (`support/stock_git.rs`):
//!
//! * `execute_commands()` runs `check_connected()` over every pushed tip
//!   (builtin/receive-pack.c:2034-2059) — a `rev-list --objects --stdin --not
//!   --exclude-hidden=receive --all --quiet --alternate-refs` child whose stderr
//!   goes down band 2 — and, when it fails, `set_connectivity_errors()` asks again
//!   tip by tip (:1773-1799), with the child's stderr on receive-pack's own and no
//!   `--exclude-hidden`, refusing each failing command with `missing necessary
//!   objects`. `--skip-connectivity-check` turns it off.
//! * `rev-list --alternate-refs` dies `bad object .alternate` on an alternate's
//!   ref it cannot read (revision.c:359-369, :1873-1878), which a hidden ref to a
//!   missing object reaches through the quarantine's alternate.
//! * `check_aliased_updates()` (:1685-1761): a command naming a symref to a ref
//!   another command writes is skipped when the two agree and both are refused
//!   with `inconsistent aliased update` when they do not; an unresolvable symref
//!   is `broken symref`. A lone update through a symref writes its target.
//! * `read_head_info()` takes the feature list from the first command, after the
//!   `shallow` lines a shallow client sends (:2192-2246).
//!
//! The raw cases hand both binaries' `receive-pack` the same request bytes (a
//! pack `pack-objects` wrote from an explicit object list, so it can leave a blob
//! out); the push cases run stock `git push` and swap only `--receive-pack`.
//! `receive.autogc` is off in every fixture: the maintenance child stock starts
//! after the report has its own diagnostics about a broken ref.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");
const NULL: &str = "0000000000000000000000000000000000000000";

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git_in(bin: &str, cwd: &Path, root: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(bin)
        .args(["-c", "maintenance.auto=false"])
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut sink = child.stdin.take().unwrap();
    let _ = sink.write_all(stdin);
    drop(sink);
    child.wait_with_output().unwrap()
}

fn git(bin: &str, cwd: &Path, root: &Path, args: &[&str]) -> Output {
    git_in(bin, cwd, root, args, b"")
}

fn ok(bin: &str, cwd: &Path, root: &Path, args: &[&str]) -> String {
    let out = git(bin, cwd, root, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// `src` with `one` (file `f`) and `two` (adds `g`) on `main` and `side` (adds
/// `h`) off `one`; `dst` a bare clone taken at `one`, with `receive.autogc` off,
/// then `setup` run in it.
fn build(stock: &str, root: &Path, setup: &[&[&str]]) {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).unwrap();
    let src = root.join("src");
    ok(stock, root, root, &["init", "-q", "-b", "main", "src"]);
    std::fs::write(src.join("f"), "a\n").unwrap();
    ok(stock, &src, root, &["add", "f"]);
    ok(stock, &src, root, &["commit", "-q", "-m", "one"]);
    ok(stock, root, root, &["clone", "-q", "--bare", "--no-local", "src", "dst"]);
    std::fs::write(src.join("g"), "g\n").unwrap();
    ok(stock, &src, root, &["add", "g"]);
    ok(stock, &src, root, &["commit", "-q", "-m", "two"]);
    ok(stock, &src, root, &["checkout", "-q", "-b", "side", "main~1"]);
    std::fs::write(src.join("h"), "h\n").unwrap();
    ok(stock, &src, root, &["add", "h"]);
    ok(stock, &src, root, &["commit", "-q", "-m", "side"]);
    ok(stock, &src, root, &["checkout", "-q", "main"]);
    let dst = root.join("dst");
    ok(stock, &dst, root, &["config", "receive.autogc", "false"]);
    for args in setup {
        ok(stock, &dst, root, args);
    }
}

/// What `dst` is left holding: its refs (symrefs named), its reflogs, and fsck.
fn state(stock: &str, root: &Path) -> String {
    let dst = root.join("dst");
    let refs = ok(stock, &dst, root, &["for-each-ref", "--format=%(refname) %(objectname) %(symref)"]);
    let mut logs = String::new();
    let mut stack = vec![dst.join("logs")];
    let mut files = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    for file in files {
        logs += &format!("{}:\n{}", file.strip_prefix(&dst).unwrap().display(), std::fs::read_to_string(&file).unwrap());
    }
    let fsck = git(stock, &dst, root, &["fsck"]);
    format!(
        "--- refs\n{refs}\n--- logs\n{logs}--- fsck\n{}{}",
        String::from_utf8_lossy(&fsck.stdout),
        String::from_utf8_lossy(&fsck.stderr)
    )
}

fn pkt(out: &mut Vec<u8>, payload: &[u8]) {
    out.extend_from_slice(format!("{:04x}", payload.len() + 4).as_bytes());
    out.extend_from_slice(payload);
}

/// One raw push: `commands` (`<old> <new> <ref>`, names resolved in `src`, `0` for
/// the null id) with `caps` on the first, then a pack of exactly `objects`.
fn raw_push(
    stock: &str,
    root: &Path,
    server: &str,
    setup: &[&[&str]],
    caps: &str,
    commands: &[(&str, &str, &str)],
    objects: &[&str],
    flags: &[&str],
) -> String {
    build(stock, root, setup);
    let src = root.join("src");
    let id = |name: &str| if name == "0" { NULL.to_string() } else { ok(stock, &src, root, &["rev-parse", name]) };
    let mut request = Vec::new();
    for (n, (old, new, name)) in commands.iter().enumerate() {
        let mut line = format!("{} {} {name}", id(old), id(new)).into_bytes();
        if n == 0 {
            line.push(0);
            line.extend_from_slice(caps.as_bytes());
        }
        pkt(&mut request, &line);
    }
    request.extend_from_slice(b"0000");
    let list: String = objects.iter().map(|o| id(o) + "\n").collect();
    let pack = git_in(stock, &src, root, &["pack-objects", "-q", "--stdout"], list.as_bytes());
    assert!(pack.status.success());
    request.extend_from_slice(&pack.stdout);

    let mut args = vec!["receive-pack"];
    args.extend(flags);
    args.push("dst");
    let out = git_in(server, root, root, &args, &request);
    format!(
        "exit {:?}\n--- stdout\n{}\n--- stderr\n{}{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).escape_debug(),
        String::from_utf8_lossy(&out.stderr),
        state(stock, root),
    )
}

/// `git push` from `src` (stock) into `dst`, served by `server`.
fn client_push(stock: &str, root: &Path, server: &str, setup: &[&[&str]], push_args: &[&str]) -> String {
    build(stock, root, setup);
    let src = root.join("src");
    let receive_pack = format!("--receive-pack={server} receive-pack");
    let mut args = vec!["push", receive_pack.as_str(), "../dst"];
    args.extend(push_args);
    let out = git(stock, &src, root, &args);
    format!(
        "exit {:?}\n--- stdout\n{}--- stderr\n{}{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        state(stock, root),
    )
}

fn base(name: &str) -> Fixture {
    Fixture {
        root: std::env::temp_dir().join(format!("zvcs-receive-connectivity-{name}-{}", std::process::id())),
    }
}

/// `main` to `two` and `side` created, the pack missing `two`'s new blob `g`;
/// a delete and a create of an existing commit ride along.
const MISSING_BLOB_COMMANDS: &[(&str, &str, &str)] = &[
    ("main~1", "main", "refs/heads/main"),
    ("0", "side", "refs/heads/side"),
    ("main~1", "0", "refs/heads/del"),
    ("0", "main~1", "refs/heads/new"),
];
const MISSING_BLOB_OBJECTS: &[&str] = &["main", "main^{tree}", "side", "side^{tree}", "side:h"];
const DEL: &[&[&str]] = &[&["update-ref", "refs/heads/del", "main"]];

fn compare_raw(name: &str, setup: &[&[&str]], caps: &str, commands: &[(&str, &str, &str)], objects: &[&str], flags: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let fx = base(name);
    let want = raw_push(stock, &fx.root, stock, setup, caps, commands, objects, flags);
    let got = raw_push(stock, &fx.root, ZVCS, setup, caps, commands, objects, flags);
    assert_eq!(got, want, "{name}");
}

fn compare_push(name: &str, setup: &[&[&str]], push_args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let fx = base(name);
    let want = client_push(stock, &fx.root, stock, setup, push_args);
    let got = client_push(stock, &fx.root, ZVCS, setup, push_args);
    assert_eq!(got, want, "{name}");
}

#[test]
fn a_tip_missing_a_blob_is_missing_necessary_objects() {
    compare_raw("blob", DEL, "report-status", MISSING_BLOB_COMMANDS, MISSING_BLOB_OBJECTS, &[]);
}

#[test]
fn the_joint_check_reports_on_band_two_and_the_singletons_on_stderr() {
    compare_raw("blob-sb", DEL, "report-status side-band-64k", MISSING_BLOB_COMMANDS, MISSING_BLOB_OBJECTS, &[]);
}

#[test]
fn a_kept_pack_missing_a_tree_is_refused_too() {
    compare_raw(
        "tree-kept",
        &[&["config", "receive.unpackLimit", "1"]],
        "report-status side-band-64k quiet",
        &[("main~1", "main", "refs/heads/main")],
        &["main"],
        &[],
    );
}

#[test]
fn skip_connectivity_check_takes_the_tip_as_it_is() {
    compare_raw(
        "skip",
        &[],
        "report-status",
        &[("main~1", "main", "refs/heads/main")],
        &["main", "main^{tree}"],
        &["--skip-connectivity-check"],
    );
}

#[test]
fn a_ref_to_a_missing_object_fails_every_tip() {
    let Some(stock) = stock_git() else { return };
    let fx = base("ghost-push");
    // `refs/heads/ghost` names `side`, which `dst` does not have; it is written
    // by hand, as no ref command will point a ref at a missing object.
    let run = |server: &str| {
        build(stock, &fx.root, &[]);
        let side = ok(stock, &fx.root.join("src"), &fx.root, &["rev-parse", "side"]);
        std::fs::write(fx.root.join("dst/refs/heads/ghost"), format!("{side}\n")).unwrap();
        let src = fx.root.join("src");
        ok(stock, &src, &fx.root, &["checkout", "-q", "-b", "kid", "side"]);
        std::fs::write(src.join("k"), "k\n").unwrap();
        ok(stock, &src, &fx.root, &["add", "k"]);
        ok(stock, &src, &fx.root, &["commit", "-q", "-m", "kid"]);
        let receive_pack = format!("--receive-pack={server} receive-pack");
        let out = git(stock, &src, &fx.root, &["push", receive_pack.as_str(), "../dst", "kid"]);
        format!(
            "exit {:?}\n--- stderr\n{}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr),
            state(stock, &fx.root)
        )
    };
    let want = run(stock);
    let got = run(ZVCS);
    assert_eq!(got, want);
}

#[test]
fn a_hidden_ref_to_a_missing_object_is_a_bad_alternate() {
    let Some(stock) = stock_git() else { return };
    let fx = base("ghost-hidden");
    let run = |server: &str| {
        build(stock, &fx.root, &[&["config", "receive.hideRefs", "refs/heads/ghost"]]);
        let src = fx.root.join("src");
        let side = ok(stock, &src, &fx.root, &["rev-parse", "side"]);
        std::fs::write(fx.root.join("dst/refs/heads/ghost"), format!("{side}\n")).unwrap();
        ok(stock, &src, &fx.root, &["checkout", "-q", "-b", "kid", "side"]);
        std::fs::write(src.join("k"), "k\n").unwrap();
        ok(stock, &src, &fx.root, &["add", "k"]);
        ok(stock, &src, &fx.root, &["commit", "-q", "-m", "kid"]);
        let kid = ok(stock, &src, &fx.root, &["rev-parse", "kid"]);
        let mut request = Vec::new();
        let mut line = format!("{NULL} {kid} refs/heads/kid").into_bytes();
        line.extend_from_slice(b"\0report-status side-band-64k");
        pkt(&mut request, &line);
        request.extend_from_slice(b"0000");
        let list: String = ["kid", "kid^{tree}", "kid:k"]
            .iter()
            .map(|o| ok(stock, &src, &fx.root, &["rev-parse", o]) + "\n")
            .collect();
        let pack = git_in(stock, &src, &fx.root, &["pack-objects", "-q", "--stdout"], list.as_bytes());
        request.extend_from_slice(&pack.stdout);
        let out = git_in(server, &fx.root, &fx.root, &["receive-pack", "dst"], &request);
        format!(
            "exit {:?}\n--- stdout\n{}\n--- stderr\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).escape_debug(),
            String::from_utf8_lossy(&out.stderr),
        )
    };
    let want = run(stock);
    let got = run(ZVCS);
    assert_eq!(got, want);
}

const ALIAS: &[&[&str]] = &[
    &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
    &["config", "core.logAllRefUpdates", "always"],
];

#[test]
fn an_inconsistent_aliased_update_refuses_both_commands() {
    compare_push("alias-inconsistent", ALIAS, &["main", "side:alias"]);
}

#[test]
fn an_inconsistent_aliased_update_fails_an_atomic_push() {
    compare_push("alias-atomic", ALIAS, &["--atomic", "main", "side:alias"]);
}

#[test]
fn a_consistent_aliased_update_writes_the_target_once() {
    compare_push("alias-consistent", ALIAS, &["main", "main:alias"]);
}

#[test]
fn a_lone_update_through_a_symref_writes_its_target() {
    compare_push("alias-lone", ALIAS, &["side:alias"]);
}

#[test]
fn head_and_its_branch_must_agree() {
    compare_raw(
        "head",
        &[],
        "report-status side-band-64k",
        &[("main~1", "main", "HEAD"), ("main~1", "side", "refs/heads/main")],
        &["main", "main^{tree}", "main:g", "side", "side^{tree}", "side:h"],
        &[],
    );
    compare_raw(
        "head-agree",
        &[],
        "report-status",
        &[("main~1", "main", "refs/heads/main"), ("main~1", "main", "HEAD")],
        &["main", "main^{tree}", "main:g"],
        &[],
    );
}

#[test]
fn an_unresolvable_symref_is_a_broken_symref() {
    compare_raw(
        "loop",
        &[&["symbolic-ref", "refs/heads/loop", "refs/heads/loop"]],
        "report-status side-band-64k",
        &[("main~1", "main", "refs/heads/loop"), ("main~1", "main", "refs/heads/main")],
        &["main", "main^{tree}", "main:g"],
        &[],
    );
}

#[test]
fn a_symref_chain_to_a_dangling_target_still_aliases() {
    compare_raw(
        "dangling",
        &[
            &["symbolic-ref", "refs/heads/l1", "refs/heads/l2"],
            &["symbolic-ref", "refs/heads/l2", "refs/heads/nope"],
        ],
        "report-status",
        &[("0", "side", "refs/heads/l1"), ("0", "main", "refs/heads/nope")],
        &["main", "main^{tree}", "main:g", "side", "side^{tree}", "side:h"],
        &[],
    );
}

/// A shallow clone's push opens with `shallow <oid>`, and the feature list rides
/// on the first command after it.
#[test]
fn a_shallow_push_reads_its_features_after_the_shallow_lines() {
    let Some(stock) = stock_git() else { return };
    let fx = base("shallow");
    let run = |server: &str| {
        build(stock, &fx.root, &[]);
        // `dst` gets `main` whole; the shallow clone has only its tip.
        ok(stock, &fx.root.join("src"), &fx.root, &["push", "-q", "../dst", "main"]);
        let url = format!("file://{}", fx.root.join("src").display());
        ok(stock, &fx.root, &fx.root, &["clone", "-q", "--depth", "1", url.as_str(), "shallow"]);
        let shallow = fx.root.join("shallow");
        std::fs::write(shallow.join("k"), "k\n").unwrap();
        ok(stock, &shallow, &fx.root, &["add", "k"]);
        ok(stock, &shallow, &fx.root, &["commit", "-q", "-m", "kid"]);
        let receive_pack = format!("--receive-pack={server} receive-pack");
        let out = git(stock, &shallow, &fx.root, &["push", receive_pack.as_str(), "../dst", "main", "main:refs/heads/new"]);
        format!(
            "exit {:?}\n--- stderr\n{}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr),
            state(stock, &fx.root)
        )
    };
    let want = run(stock);
    let got = run(ZVCS);
    assert_eq!(got, want);
}

#[test]
fn rev_list_alternate_refs_dies_on_a_missing_tip() {
    let Some(stock) = stock_git() else { return };
    let fx = base("alternate");
    build(stock, &fx.root, &[]);
    let side = ok(stock, &fx.root.join("src"), &fx.root, &["rev-parse", "side"]);
    std::fs::write(fx.root.join("dst/refs/heads/ghost"), format!("{side}\n")).unwrap();
    ok(stock, &fx.root, &fx.root, &["init", "-q", "--bare", "borrower"]);
    std::fs::write(
        fx.root.join("borrower/objects/info/alternates"),
        format!("{}\n", fx.root.join("dst/objects").display()),
    )
    .unwrap();
    let borrower = fx.root.join("borrower");
    let run = |bin: &str| {
        let out = git(bin, &borrower, &fx.root, &["rev-list", "--all", "--alternate-refs"]);
        format!(
            "exit {:?}\n{}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    };
    assert_eq!(run(ZVCS), run(stock));
}

/// A tag on a blob or a tree is a tip the check feeds rev-list like any other;
/// `handle_commit()`'s tag loop pends what it peels to rather than failing the
/// operand (revision.c), so `push --tags` of such tags goes through.
#[test]
fn tags_on_a_blob_and_a_tree_pass_the_check() {
    let Some(stock) = stock_git() else { return };
    let fx = base("tags");
    let run = |server: &str| {
        build(stock, &fx.root, &[]);
        let src = fx.root.join("src");
        let blob = ok(stock, &src, &fx.root, &["rev-parse", "main:g"]);
        let tree = ok(stock, &src, &fx.root, &["rev-parse", "main^{tree}"]);
        ok(stock, &src, &fx.root, &["tag", "-a", "blobtag", "-m", "on a blob", &blob]);
        ok(stock, &src, &fx.root, &["tag", "-a", "treetag", "-m", "on a tree", &tree]);
        let receive_pack = format!("--receive-pack={server} receive-pack");
        let out = git(stock, &src, &fx.root, &["push", receive_pack.as_str(), "--tags", "../dst", "main"]);
        format!(
            "exit {:?}\n--- stderr\n{}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr),
            state(stock, &fx.root)
        )
    };
    let want = run(stock);
    let got = run(ZVCS);
    assert_eq!(got, want);
}

#[test]
fn rev_list_takes_a_tag_on_a_blob_as_an_operand() {
    let Some(stock) = stock_git() else { return };
    let fx = base("tag-operand");
    build(stock, &fx.root, &[]);
    let src = fx.root.join("src");
    let blob = ok(stock, &src, &fx.root, &["rev-parse", "main:g"]);
    let tree = ok(stock, &src, &fx.root, &["rev-parse", "main^{tree}"]);
    ok(stock, &src, &fx.root, &["tag", "-a", "blobtag", "-m", "on a blob", &blob]);
    ok(stock, &src, &fx.root, &["tag", "-a", "treetag", "-m", "on a tree", &tree]);
    let tag = ok(stock, &src, &fx.root, &["rev-parse", "blobtag"]);
    let cases: [&[&str]; 4] = [
        &["rev-list", "--objects", &tag],
        &["rev-list", "--objects", "treetag", "^blobtag", "main"],
        &["rev-list", "blobtag"],
        &["rev-list", "--objects", "--stdin"],
    ];
    let stdin = format!("{tag}\ntreetag\n");
    for args in cases {
        let run = |bin: &str| {
            let out = git_in(bin, &src, &fx.root, args, stdin.as_bytes());
            format!(
                "exit {:?}\n{}{}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        };
        assert_eq!(run(ZVCS), run(stock), "{args:?}");
    }
}
