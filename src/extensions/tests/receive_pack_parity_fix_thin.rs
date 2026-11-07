//! What a push leaves in the receiving repository: `receive-pack` hands the pack
//! to `index-pack --stdin --fix-thin` or `unpack-objects` (object-file.c:1366-1445),
//! and `index-pack` resolves the pack's REF_DELTAs against the pack itself before
//! it appends, at the end, only the bases still missing (`conclude_pack()`,
//! `fix_unresolved_deltas()`, builtin/index-pack.c:1370-1541). A base the pack
//! already carries, even as a delta, is never stored twice, even when the
//! receiving repository has it too.
//!
//! Every case runs the same push twice from identical fixtures, once into stock
//! `receive-pack` and once into this binary's, and compares the pusher's output
//! and what the server keeps: each pack by name with its objects in pack order
//! (`verify-pack -v`), the files beside it and their modes, the loose objects,
//! `fsck` and the refs. Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

#[path = "support/cgi_server.rs"]
mod cgi_server;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git_in(bin: &str, cwd: &Path, root: &Path, stdin: Option<&Path>, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .stdin(match stdin {
            Some(path) => Stdio::from(std::fs::File::open(path).unwrap()),
            None => Stdio::null(),
        })
        .output()
        .unwrap()
}

fn git(bin: &str, cwd: &Path, root: &Path, args: &[&str]) -> std::process::Output {
    git_in(bin, cwd, root, None, args)
}

/// `srv/srv.git` holds `c1..c6` and `B` on top as `main`; `loc` was cloned at `c6`
/// and has `topic` = `Y` on top of it, so it lacks the advertised tip and, without
/// negotiation, sends the whole history, trees the server already has included.
/// With `bad`, `topic` is a commit whose author line `fsck` rejects.
fn fixture(stock: &str, tag: &str, server_config: &[(&str, &str)], bad: bool) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-rpfixthin-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("srv")).unwrap();
    let root = root.canonicalize().unwrap();
    let ok = |cwd: &Path, args: &[&str]| -> String {
        let out = git(stock, cwd, &root, args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    ok(&root, &["init", "-q", "-b", "main", "seed"]);
    let seed = root.join("seed");
    for i in 1..=6 {
        std::fs::write(seed.join(format!("f{i}")), format!("{i}\n")).unwrap();
        ok(&seed, &["add", "."]);
        ok(&seed, &["commit", "-qm", &format!("c{i}")]);
    }
    ok(&root, &["clone", "-q", "--bare", "seed", "srv/srv.git"]);
    let srv = root.join("srv/srv.git");
    // No `gc --auto` after a push: it would repack what is compared.
    ok(&srv, &["config", "receive.autogc", "false"]);
    ok(&srv, &["config", "http.receivepack", "true"]);
    for (key, value) in server_config {
        ok(&srv, &["config", key, value]);
    }
    ok(&root, &["clone", "-q", "seed", "loc"]);
    std::fs::write(seed.join("b"), "B\n").unwrap();
    ok(&seed, &["add", "b"]);
    ok(&seed, &["commit", "-qm", "B"]);
    ok(&seed, &["push", "-q", "../srv/srv.git", "main"]);
    let loc = root.join("loc");
    ok(&loc, &["checkout", "-q", "-b", "topic"]);
    std::fs::write(loc.join("y"), "Y\n").unwrap();
    ok(&loc, &["add", "y"]);
    ok(&loc, &["commit", "-qm", "Y"]);
    if bad {
        let tree = ok(&loc, &["rev-parse", "HEAD^{tree}"]);
        let parent = ok(&loc, &["rev-parse", "HEAD"]);
        let body = format!(
            "tree {tree}\nparent {parent}\nauthor bad <a@x 1700000000 +0000\ncommitter a <a@x> 1700000000 +0000\n\nbad\n"
        );
        let path = root.join("bad-commit");
        std::fs::write(&path, body).unwrap();
        let out = git_in(stock, &loc, &root, Some(&path), &["hash-object", "-t", "commit", "-w", "--literally", "--stdin"]);
        let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        ok(&loc, &["update-ref", "refs/heads/topic", &id]);
    }
    ok(&loc, &["remote", "set-url", "origin", "../srv/srv.git"]);
    Fixture { root }
}

/// Everything the receiving repository holds after the push.
fn server_state(stock: &str, root: &Path) -> String {
    let srv = root.join("srv/srv.git");
    let mut state = String::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(srv.join("objects/pack"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    for path in &files {
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        state += &format!("{} {mode:o}\n", path.file_name().unwrap().to_string_lossy());
        if path.extension().is_some_and(|e| e == "idx") {
            let out = git(stock, &srv, root, &["verify-pack", "-v", path.to_str().unwrap()]);
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() >= 2 && fields[0].len() >= 40 && fields[0].bytes().all(|b| b.is_ascii_hexdigit()) {
                    state += &format!("  {} {}\n", fields[0], fields[1]);
                }
            }
            state += &format!("  verify-pack: {}\n", out.status.code().unwrap_or(-1));
        }
    }
    let mut loose = Vec::new();
    for dir in std::fs::read_dir(srv.join("objects")).unwrap().flatten() {
        let name = dir.file_name().to_string_lossy().into_owned();
        if name.len() == 2 {
            for obj in std::fs::read_dir(dir.path()).unwrap().flatten() {
                loose.push(format!("{name}{}", obj.file_name().to_string_lossy()));
            }
        }
    }
    loose.sort();
    state += &format!("loose: {loose:?}\n");
    let fsck = git(stock, &srv, root, &["fsck", "--no-dangling"]);
    state += &format!(
        "fsck {:?}: {}{}",
        fsck.status.code(),
        String::from_utf8_lossy(&fsck.stdout),
        String::from_utf8_lossy(&fsck.stderr)
    );
    state += &String::from_utf8_lossy(&git(stock, &srv, root, &["for-each-ref"]).stdout);
    state
}

/// A `receive-pack` program for the client to run: `bin receive-pack`.
fn receive_pack_program(root: &Path, bin: &str, name: &str) -> String {
    let path = root.join(name);
    std::fs::write(&path, format!("#!/bin/sh\nexec '{bin}' receive-pack \"$@\"\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_str().unwrap().to_owned()
}

#[derive(Clone, Copy)]
struct Case<'a> {
    client: &'a str,
    negotiate: bool,
    server_config: &'a [(&'a str, &'a str)],
    bad: bool,
}

/// Push `topic` with `case.client` into a server whose `receive-pack` is `server`.
fn push(stock: &str, server: &str, tag: &str, case: Case) -> (Option<i32>, String, String, String) {
    let fx = fixture(stock, tag, case.server_config, case.bad);
    let root = &fx.root;
    let program = receive_pack_program(root, server, "receive-pack");
    let negotiate = format!("push.negotiate={}", case.negotiate);
    let out = git(
        case.client,
        &root.join("loc"),
        root,
        &["-c", &negotiate, "push", "--receive-pack", &program, "origin", "topic"],
    );
    let root_s = root.to_str().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).replace(root_s, "ROOT"),
        String::from_utf8_lossy(&out.stderr).replace(root_s, "ROOT"),
        server_state(stock, root),
    )
}

fn compare(stock: &str, tag: &str, case: Case) {
    let want = push(stock, stock, &format!("{tag}-s"), case);
    let got = push(stock, ZVCS, &format!("{tag}-z"), case);
    assert_eq!(got, want, "{tag}: zvcs receive-pack and stock receive-pack differ");
}

#[test]
fn receive_pack_parity_kept_pack_holds_each_object_once() {
    let Some(stock) = stock_git() else { return };
    let keep: &[(&str, &str)] = &[("receive.unpackLimit", "1")];
    let loose: &[(&str, &str)] = &[("receive.unpackLimit", "100")];
    for (client, who) in [(stock, "stock"), (ZVCS, "zvcs")] {
        for negotiate in [false, true] {
            for (server_config, limit) in [(keep, "keep"), (loose, "loose")] {
                let tag = format!("{who}-{negotiate}-{limit}");
                compare(stock, &tag, Case { client, negotiate, server_config, bad: false });
            }
        }
    }
}

#[test]
fn receive_pack_parity_fsck_objects() {
    let Some(stock) = stock_git() else { return };
    for (server_config, tag) in [
        (&[("receive.unpackLimit", "1"), ("receive.fsckObjects", "true")][..], "keep-receive"),
        (&[("receive.unpackLimit", "100"), ("receive.fsckObjects", "true")][..], "loose-receive"),
        (&[("receive.unpackLimit", "1"), ("transfer.fsckObjects", "true")][..], "keep-transfer"),
    ] {
        for bad in [false, true] {
            let case = Case { client: stock, negotiate: true, server_config, bad };
            compare(stock, &format!("fsck-{tag}-{bad}"), case);
        }
    }
}

#[test]
fn receive_pack_parity_no_reverse_index() {
    let Some(stock) = stock_git() else { return };
    let server_config: &[(&str, &str)] = &[("receive.unpackLimit", "1"), ("pack.writeReverseIndex", "false")];
    compare(stock, "norev", Case { client: stock, negotiate: true, server_config, bad: false });
}

#[test]
fn receive_pack_parity_thin_push_over_smart_http() {
    let Some(stock) = stock_git() else { return };
    let server_config: &[(&str, &str)] = &[("receive.unpackLimit", "1")];
    for (client, who) in [(stock, "stock"), (ZVCS, "zvcs")] {
        let mut runs = Vec::new();
        for (server, side) in [(stock, "s"), (ZVCS, "z")] {
            let fx = fixture(stock, &format!("http-{who}-{side}"), server_config, false);
            let (port, _posts) = cgi_server::spawn(server, fx.root.join("srv"), cgi_server::Fault::NONE);
            let url = format!("http://127.0.0.1:{port}/srv.git");
            let out = git(client, &fx.root.join("loc"), &fx.root, &["-c", "push.negotiate=true", "push", &url, "topic"]);
            let host = format!("127.0.0.1:{port}");
            runs.push((
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).replace(&host, "HOST"),
                String::from_utf8_lossy(&out.stderr).replace(&host, "HOST"),
                server_state(stock, &fx.root),
            ));
        }
        assert_eq!(runs[1], runs[0], "{who} client: zvcs http-backend and stock http-backend differ");
    }
}

/// `index-pack --stdin --fix-thin` run directly in the server of a fresh fixture,
/// on a pack stock `pack-objects` built in the client from `revs`.
fn index_pack(stock: &str, bin: &str, tag: &str, pack_args: &[&str], revs: &str) -> (Option<i32>, String, String, String) {
    let fx = fixture(stock, tag, &[], false);
    let root = &fx.root;
    let revs_path = root.join("revs");
    std::fs::write(&revs_path, revs).unwrap();
    let pack = git_in(stock, &root.join("loc"), root, Some(&revs_path), pack_args);
    assert!(pack.status.success(), "{}", String::from_utf8_lossy(&pack.stderr));
    let pack_path = root.join("in.pack");
    std::fs::write(&pack_path, &pack.stdout).unwrap();
    let out = git_in(bin, &root.join("srv/srv.git"), root, Some(&pack_path), &["index-pack", "--stdin", "--fix-thin"]);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        server_state(stock, root),
    )
}

#[test]
fn index_pack_parity_fix_thin_resolves_in_pack_bases_first() {
    let Some(stock) = stock_git() else { return };
    // Without `--delta-base-offset` every delta names its base by id, and the
    // trees' bases are in the pack as well as in the receiving repository.
    let ref_deltas = ["pack-objects", "--revs", "--stdout", "-q"];
    // `--thin` with the history the server has excluded: the bases are only there.
    let thin = ["pack-objects", "--revs", "--stdout", "-q", "--thin"];
    let thin_ofs = ["pack-objects", "--revs", "--stdout", "-q", "--thin", "--delta-base-offset"];
    for (args, revs, tag) in [
        (&ref_deltas[..], "topic\n", "ref"),
        (&thin[..], "topic\n^main~1\n", "thin"),
        (&thin_ofs[..], "topic\n^main~1\n", "thin-ofs"),
    ] {
        let want = index_pack(stock, stock, &format!("ip-{tag}-s"), args, revs);
        let got = index_pack(stock, ZVCS, &format!("ip-{tag}-z"), args, revs);
        assert_eq!(got, want, "{tag}: zvcs index-pack and stock index-pack differ");
    }
}
