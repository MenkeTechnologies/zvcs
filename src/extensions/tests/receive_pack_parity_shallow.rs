//! `receive-pack`'s shallow-push handling and its delete through a symref,
//! against stock git as the oracle (`support/stock_git.rs`):
//!
//! * `prepare_shallow_info()` / `update_shallow_info()` (shallow.c:516-892,
//!   builtin/receive-pack.c:2373-2409): a push from a shallow clone whose
//!   boundary commit the server lacks is refused per ref with `shallow update
//!   not allowed`, while a ref that needs no shallow commit goes through.
//! * `receive.shallowUpdate` (`prepare_shallow_update()`, `update_shallow_ref()`,
//!   :1332-1366, :2329-2371): the hooks see `GIT_SHALLOW_FILE`, the boundary is
//!   added to the server's `.git/shallow` (an empty one when every boundary is
//!   already reachable), and stock's `command_singleton_iterator` misuse checks
//!   the *next* command's tip and cuts the command list after the first ref
//!   that needed shallow commits. A push of deletes alone dies of SIGSEGV.
//! * A delete through a symref deletes the ref it points at and logs on the
//!   symref and on `HEAD` (`split_symref_update()`, refs/files-backend.c:2555-2607),
//!   over a local path and `file://`, and so does `update-ref -d`.
//!
//! Every push is stock `git push` with only `--receive-pack` swapped; the raw
//! cases hand both binaries' `receive-pack` the same request bytes.
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

fn fixture(name: &str) -> Fixture {
    Fixture {
        root: std::env::temp_dir().join(format!("zvcs-receive-shallow-{name}-{}", std::process::id())),
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

fn commit(stock: &str, repo: &Path, root: &Path, file: &str) {
    std::fs::write(repo.join(file), format!("{file}\n")).unwrap();
    ok(stock, repo, root, &["add", file]);
    ok(stock, repo, root, &["commit", "-q", "-m", file]);
}

/// `up` with `f1`..`f3` on `main`; `client`, a `--depth 1` clone of it with
/// `f4` on top, `side` at the same commit and an unrelated `orph`; `dst`, an
/// empty bare repository (or, with `clone_dst`, a bare clone of `up`) with
/// `receive.autogc` off, then `setup` run in it.
fn build(stock: &str, root: &Path, clone_dst: bool, setup: &[&[&str]]) {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).unwrap();
    let up = root.join("up");
    ok(stock, root, root, &["init", "-q", "-b", "main", "up"]);
    for f in ["f1", "f2", "f3"] {
        commit(stock, &up, root, f);
    }
    let url = format!("file://{}", up.display());
    ok(stock, root, root, &["clone", "-q", "--depth", "1", url.as_str(), "client"]);
    let client = root.join("client");
    commit(stock, &client, root, "f4");
    ok(stock, &client, root, &["branch", "side"]);
    ok(stock, &client, root, &["checkout", "-q", "--orphan", "orph"]);
    commit(stock, &client, root, "o");
    ok(stock, &client, root, &["checkout", "-q", "main"]);
    if clone_dst {
        ok(stock, root, root, &["clone", "-q", "--bare", "--no-local", "up", "dst"]);
    } else {
        ok(stock, root, root, &["init", "-q", "--bare", "dst"]);
    }
    let dst = root.join("dst");
    ok(stock, &dst, root, &["config", "receive.autogc", "false"]);
    for args in setup {
        ok(stock, &dst, root, args);
    }
}

/// What `dst` is left holding: its refs, its shallow file (or its absence),
/// its reflogs, fsck, and the names in its git directory, so a leftover
/// `shallow_XXXXXX` or `shallow.lock` shows.
fn state(stock: &str, root: &Path) -> String {
    let dst = root.join("dst");
    let refs = ok(stock, &dst, root, &["for-each-ref", "--format=%(refname) %(objectname) %(symref)"]);
    let shallow = match std::fs::read_to_string(dst.join("shallow")) {
        Ok(text) => format!("present\n{text}"),
        Err(_) => "absent\n".to_string(),
    };
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
    let mut names: Vec<String> = std::fs::read_dir(&dst)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let fsck = git(stock, &dst, root, &["fsck"]);
    format!(
        "--- refs\n{refs}\n--- shallow {shallow}--- logs\n{logs}--- names\n{}\n--- fsck\n{}{}",
        names.join(" "),
        String::from_utf8_lossy(&fsck.stdout),
        String::from_utf8_lossy(&fsck.stderr)
    )
}

/// The random six characters of a `shallow_XXXXXX` tempfile name.
fn mask_tempfile(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("shallow_") {
        out.push_str(&rest[..at + 8]);
        rest = &rest[at + 8..];
        let n = rest.chars().take_while(char::is_ascii_alphanumeric).count();
        if n == 6 {
            out.push_str("XXXXXX");
            rest = &rest[6..];
        }
    }
    out.push_str(rest);
    out
}

/// `git push` from `client` (stock) into `dst` over `url` (`path` or `file`),
/// served by `server`.
fn client_push(
    stock: &str,
    root: &Path,
    server: &str,
    clone_dst: bool,
    setup: &[&[&str]],
    url: &str,
    push_args: &[&str],
) -> String {
    build(stock, root, clone_dst, setup);
    push_only(stock, root, server, url, push_args)
}

/// The push of [`client_push`] into an already built fixture.
fn push_only(stock: &str, root: &Path, server: &str, url: &str, push_args: &[&str]) -> String {
    let client = root.join("client");
    let dst = root.join("dst");
    let target = match url {
        "file" => format!("file://{}", dst.display()),
        _ => dst.display().to_string(),
    };
    let receive_pack = format!("--receive-pack={server} receive-pack");
    let mut args = vec!["push", "--porcelain", receive_pack.as_str(), target.as_str()];
    args.extend(push_args);
    let out = git(stock, &client, root, &args);
    mask_tempfile(&format!(
        "exit {:?}\n--- stdout\n{}--- stderr\n{}{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        state(stock, root),
    ))
}

fn compare_push(name: &str, clone_dst: bool, setup: &[&[&str]], url: &str, push_args: &[&str]) {
    let Some(stock) = stock_git() else { return };
    let fx = fixture(name);
    let want = client_push(stock, &fx.root, stock, clone_dst, setup, url, push_args);
    let got = client_push(stock, &fx.root, ZVCS, clone_dst, setup, url, push_args);
    assert_eq!(got, want, "{name}");
}

const SHALLOW_UPDATE: &[&[&str]] = &[&["config", "receive.shallowUpdate", "true"]];

#[test]
fn a_boundary_the_server_lacks_is_shallow_update_not_allowed() {
    compare_push("refused", false, &[], "path", &["main"]);
    compare_push("refused-file", false, &[], "file", &["main", "orph"]);
}

#[test]
fn shallow_update_writes_the_boundary_into_the_servers_shallow_file() {
    compare_push("update", false, SHALLOW_UPDATE, "path", &["main"]);
    compare_push("update-file", false, SHALLOW_UPDATE, "file", &["orph", "main"]);
}

#[test]
fn a_boundary_the_server_reaches_needs_no_shallow_update() {
    compare_push("reached", true, &[], "path", &["main"]);
}

#[test]
fn a_boundary_the_server_has_but_does_not_reach_is_refused() {
    compare_push("unreached", true, &[&["update-ref", "-d", "refs/heads/main"]], "path", &["main"]);
}

#[test]
fn update_shallow_ref_checks_the_next_command_and_cuts_the_list() {
    compare_push("cut", false, SHALLOW_UPDATE, "path", &["main", "side", "orph"]);
    compare_push("cut-atomic", false, SHALLOW_UPDATE, "file", &["--atomic", "main", "orph"]);
}

#[test]
fn a_boundary_every_ref_already_reaches_leaves_an_empty_shallow_file() {
    let Some(stock) = stock_git() else { return };
    let fx = fixture("empty");
    // `dst` moves two commits past the client's boundary, so the boundary is
    // reachable from `dst`'s `main` without being one of its tips or their parents.
    let run = |server: &str| {
        build(stock, &fx.root, true, SHALLOW_UPDATE);
        let up = fx.root.join("up");
        commit(stock, &up, &fx.root, "x");
        commit(stock, &up, &fx.root, "y");
        ok(stock, &fx.root.join("dst"), &fx.root, &["fetch", "-q", "../up", "+main:main"]);
        let client = fx.root.join("client");
        let receive_pack = format!("--receive-pack={server} receive-pack");
        let out = git(stock, &client, &fx.root, &["push", "--porcelain", receive_pack.as_str(), "../dst", "main:refs/heads/new", "side"]);
        format!(
            "exit {:?}\n--- stdout\n{}--- stderr\n{}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            state(stock, &fx.root)
        )
    };
    let want = run(stock);
    let got = run(ZVCS);
    assert_eq!(got, want);
}

#[test]
fn hooks_see_the_temporary_shallow_file_only_under_shallow_update() {
    let Some(stock) = stock_git() else { return };
    for (name, update) in [("hooks", true), ("hooks-off", false)] {
        let fx = fixture(name);
        let run = |server: &str| {
            let setup: &[&[&str]] = if update { SHALLOW_UPDATE } else { &[] };
            build(stock, &fx.root, false, setup);
            let hooks = fx.root.join("dst/hooks");
            let body = "#!/bin/sh\necho \"[$GIT_SHALLOW_FILE]\"\ncat \"$GIT_SHALLOW_FILE\" 2>&1\ngit rev-list --count --all\nexit 0\n";
            for hook in ["pre-receive", "update", "post-receive", "post-update"] {
                let path = hooks.join(hook);
                std::fs::write(&path, body).unwrap();
                std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
            }
            push_only(stock, &fx.root, server, "path", &["main", "orph"])
        };
        let want = run(stock);
        let got = run(ZVCS);
        assert_eq!(got, want, "{name}");
    }
}

fn pkt(out: &mut Vec<u8>, payload: &[u8]) {
    out.extend_from_slice(format!("{:04x}", payload.len() + 4).as_bytes());
    out.extend_from_slice(payload);
}

/// One raw push from `client`: its `shallow` line, then `commands` (`<new> <ref>`
/// creations, `D` for `main`, `E` for an orphan commit whose blob the pack leaves
/// out, `0` for a delete) with `caps` on the first, then a pack of `main`'s
/// objects and `E`'s commit and tree.
fn raw_push(stock: &str, root: &Path, server: &str, setup: &[&[&str]], caps: &str, commands: &[(&str, &str)]) -> String {
    build(stock, root, false, setup);
    let client = root.join("client");
    let blob = git_in(stock, &client, root, &["hash-object", "-w", "--stdin"], b"missing\n");
    let blob = String::from_utf8_lossy(&blob.stdout).trim_end().to_string();
    let tree = git_in(stock, &client, root, &["mktree"], format!("100644 blob {blob}\tm\n").as_bytes());
    let tree = String::from_utf8_lossy(&tree.stdout).trim_end().to_string();
    let e = git_in(stock, &client, root, &["commit-tree", tree.as_str()], b"e\n");
    let e = String::from_utf8_lossy(&e.stdout).trim_end().to_string();
    let d = ok(stock, &client, root, &["rev-parse", "main"]);
    let shallow = std::fs::read_to_string(client.join(".git/shallow")).unwrap();

    let mut request = Vec::new();
    pkt(&mut request, format!("shallow {shallow}").as_bytes());
    for (n, (new, name)) in commands.iter().enumerate() {
        let new = match *new {
            "D" => d.as_str(),
            "E" => e.as_str(),
            _ => NULL,
        };
        let mut line = format!("{NULL} {new} {name}").into_bytes();
        if n == 0 {
            line.push(0);
            line.extend_from_slice(caps.as_bytes());
        }
        pkt(&mut request, &line);
    }
    request.extend_from_slice(b"0000");
    let mut list = ok(stock, &client, root, &["rev-list", "--objects", "main"]);
    list += &format!("\n{e}\n{tree}\n");
    let pack = git_in(stock, &client, root, &["pack-objects", "-q", "--stdout"], list.as_bytes());
    assert!(pack.status.success());
    request.extend_from_slice(&pack.stdout);

    let out = git_in(server, root, root, &["receive-pack", "dst"], &request);
    mask_tempfile(&format!(
        "exit {:?}\n--- stdout\n{}\n--- stderr\n{}{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).escape_debug(),
        String::from_utf8_lossy(&out.stderr),
        state(stock, root),
    ))
}

fn compare_raw(name: &str, setup: &[&[&str]], caps: &str, commands: &[(&str, &str)]) {
    let Some(stock) = stock_git() else { return };
    let fx = fixture(name);
    let want = raw_push(stock, &fx.root, stock, setup, caps, commands);
    let got = raw_push(stock, &fx.root, ZVCS, setup, caps, commands);
    assert_eq!(got, want, "{name}");
}

#[test]
fn a_failing_check_of_the_next_tip_is_a_shallow_error() {
    let commands: &[(&str, &str)] = &[("D", "refs/heads/a"), ("E", "refs/heads/b"), ("D", "refs/heads/c")];
    compare_raw("next-fails", SHALLOW_UPDATE, "report-status", commands);
    compare_raw("next-fails-sb", SHALLOW_UPDATE, "report-status side-band-64k", commands);
    compare_raw("next-fails-off", &[], "report-status", commands);
}

#[test]
fn the_last_command_checks_nothing() {
    compare_raw("last", SHALLOW_UPDATE, "report-status", &[("E", "refs/heads/b"), ("D", "refs/heads/a")]);
    compare_raw("last-delete", SHALLOW_UPDATE, "report-status", &[("D", "refs/heads/a"), ("0", "refs/heads/x")]);
}

#[test]
fn a_delete_only_push_from_a_shallow_client_crashes_under_shallow_update() {
    let Some(stock) = stock_git() else { return };
    let fx = fixture("delete-only");
    let run = |server: &str| {
        build(stock, &fx.root, false, SHALLOW_UPDATE);
        // `orph` needs no shallow commit, so stock's own push puts it there.
        let receive_pack = format!("--receive-pack={stock} receive-pack");
        ok(stock, &fx.root.join("client"), &fx.root, &["push", "-q", receive_pack.as_str(), "../dst", "orph"]);
        push_only(stock, &fx.root, server, "path", &[":refs/heads/orph"])
    };
    let want = run(stock);
    let got = run(ZVCS);
    assert_eq!(got, want);
}

/// `up` cloned bare into `dst`, with `refs/heads/alias` a symref to `main`,
/// `core.logAllRefUpdates=always`, a reflog entry on `main` and `HEAD`, and a
/// packed tag `keep` that leaves `packed-refs` non-empty once `main` is gone.
fn build_alias(stock: &str, root: &Path) {
    build(stock, root, true, &[]);
    let dst = root.join("dst");
    ok(stock, &dst, root, &["config", "core.logAllRefUpdates", "always"]);
    ok(stock, &dst, root, &["tag", "keep", "main"]);
    ok(stock, &dst, root, &["pack-refs", "--all"]);
    ok(stock, &dst, root, &["symbolic-ref", "refs/heads/alias", "refs/heads/main"]);
    ok(stock, &dst, root, &["update-ref", "-m", "seed", "refs/heads/main", "refs/heads/main"]);
    ok(stock, &dst, root, &["branch", "other", "main~1"]);
}

#[test]
fn a_delete_through_a_symref_deletes_its_target() {
    let Some(stock) = stock_git() else { return };
    for url in ["path", "file"] {
        let fx = fixture(&format!("alias-{url}"));
        let run = |server: &str| {
            build_alias(stock, &fx.root);
            let up = fx.root.join("up");
            let dst = fx.root.join("dst");
            let target = if url == "file" { format!("file://{}", dst.display()) } else { dst.display().to_string() };
            let receive_pack = format!("--receive-pack={server} receive-pack");
            let out = git(stock, &up, &fx.root, &["push", "--porcelain", receive_pack.as_str(), target.as_str(), ":refs/heads/alias"]);
            format!(
                "exit {:?}\n--- stdout\n{}--- stderr\n{}{}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
                state(stock, &fx.root)
            )
        };
        let want = run(stock);
        let got = run(ZVCS);
        assert_eq!(got, want, "{url}");
    }
}

#[test]
fn an_atomic_delete_through_a_symref_rides_with_another_delete() {
    let Some(stock) = stock_git() else { return };
    let fx = fixture("alias-atomic");
    let run = |server: &str| {
        build_alias(stock, &fx.root);
        let dst = fx.root.join("dst");
        let main = ok(stock, &dst, &fx.root, &["rev-parse", "main"]);
        let other = ok(stock, &dst, &fx.root, &["rev-parse", "other"]);
        let mut request = Vec::new();
        pkt(&mut request, format!("{main} {NULL} refs/heads/alias\0report-status atomic").as_bytes());
        pkt(&mut request, format!("{other} {NULL} refs/heads/other").as_bytes());
        request.extend_from_slice(b"0000");
        let out = git_in(server, &fx.root, &fx.root, &["receive-pack", "dst"], &request);
        format!(
            "exit {:?}\n--- stdout\n{}\n--- stderr\n{}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).escape_debug(),
            String::from_utf8_lossy(&out.stderr),
            state(stock, &fx.root)
        )
    };
    let want = run(stock);
    let got = run(ZVCS);
    assert_eq!(got, want);
}

#[test]
fn update_ref_delete_through_a_symref_deletes_its_target() {
    let Some(stock) = stock_git() else { return };
    let fx = fixture("alias-update-ref");
    let run = |bin: &str| {
        build_alias(stock, &fx.root);
        let dst = fx.root.join("dst");
        let main = ok(stock, &dst, &fx.root, &["rev-parse", "main"]);
        let out = git(bin, &dst, &fx.root, &["update-ref", "-m", "drop", "-d", "refs/heads/alias", main.as_str()]);
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
