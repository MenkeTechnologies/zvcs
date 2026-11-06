//! `push.negotiate`: `send_pack()` runs `git fetch --negotiate-only` first and
//! leaves what it proves common out of the pack (`get_commons_through_negotiation()`,
//! send-pack.c:402-475).
//!
//! The child gets one `--negotiation-restrict=<oid>` per non-null `new_oid` of
//! `remote_refs` — up-to-date and rejected refs included, deletions not — unless
//! `remote.<name>.negotiationRestrict` replaces them, plus a
//! `--negotiation-include` per `remote.<name>.negotiationInclude`. `git send-pack`
//! and a push through a remote helper never set those two lists. A child that
//! fails (a protocol v0 server, a server without `wait-for-done`) leaves its own
//! message, then `warning: push negotiation failed; proceeding anyway with push`,
//! and the push goes on with the advertisement alone.
//!
//! Each fixture is a server holding most of the local history, behind a tip the
//! client does not have, so the advertisement alone proves nothing common. The
//! server keeps every pushed pack (`receive.unpackLimit=1`), and the client
//! pushes with `--no-thin` so the kept pack is what was sent: its object count is
//! what negotiation saved. The negotiation itself is read from the packet trace
//! of a stock `upload-pack` both clients are pointed at. Stock git is the oracle
//! (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::{stock_git, stock_git_at_least};

#[path = "support/cgi_server.rs"]
mod cgi_server;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

#[derive(Debug, PartialEq)]
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// `for-each-ref` of the server.
    refs: String,
    /// Object count of the newest pack the server kept.
    pack: u32,
    /// The `upload-pack< ` lines of the negotiation, `agent=` dropped (its position
    /// among the capabilities is not what this compares).
    negotiation: Vec<String>,
}

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(bin: &str, cwd: &Path, root: &Path, envs: &[(&str, &str)], args: &[&str]) -> std::process::Output {
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
        .envs(envs.iter().copied())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// `srv/srv.git` holds `c1..c6` and `B` on top as `main`; `loc` was cloned at `c6`
/// and has `topic` = `Y` on top of it, so it lacks the advertised tip.
fn fixture(stock: &str, tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-pushneg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("srv")).unwrap();
    let root = root.canonicalize().unwrap();
    let ok = |cwd: &Path, args: &[&str]| {
        let out = git(stock, cwd, &root, &[], args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    ok(&root, &["init", "-q", "-b", "main", "seed"]);
    let seed = root.join("seed");
    for i in 1..=6 {
        std::fs::write(seed.join(format!("f{i}")), format!("{i}\n")).unwrap();
        ok(&seed, &["add", "."]);
        ok(&seed, &["commit", "-qm", &format!("c{i}")]);
    }
    ok(&root, &["clone", "-q", "--bare", "seed", "srv/srv.git"]);
    ok(&root.join("srv/srv.git"), &["config", "receive.unpackLimit", "1"]);
    // No `gc --auto` after a push: it would repack what the count reads.
    ok(&root.join("srv/srv.git"), &["config", "receive.autogc", "false"]);
    ok(&root.join("srv/srv.git"), &["config", "http.receivepack", "true"]);
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
    ok(&loc, &["remote", "set-url", "origin", "../srv/srv.git"]);
    Fixture { root }
}

fn pack_count(srv: &Path) -> u32 {
    let mut packs: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(srv.join("objects/pack"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "pack"))
        .map(|p| (std::fs::metadata(&p).unwrap().modified().unwrap(), p))
        .collect();
    packs.sort();
    let bytes = std::fs::read(&packs.last().unwrap().1).unwrap();
    u32::from_be_bytes(bytes[8..12].try_into().unwrap())
}

/// Run `args` with `bin` in a fresh fixture. `setup` adjusts the fixture with stock
/// git first. `origin` receives through stock `receive-pack` for both clients, so
/// the pack the server keeps is what the client sent.
fn run(
    stock: &str,
    bin: &str,
    tag: &str,
    setup: &dyn Fn(&Path, &dyn Fn(&Path, &[&str])),
    args: &[&str],
) -> Run {
    let fx = fixture(stock, tag);
    let root = &fx.root;
    let loc = root.join("loc");
    let ok = |cwd: &Path, args: &[&str]| {
        let out = git(stock, cwd, root, &[], args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    ok(&loc, &["config", "remote.origin.receivepack", &service(stock, "receive-pack")]);
    setup(root, &ok);
    let trace = root.join("trace");
    let out = git(bin, &loc, root, &[("GIT_TRACE_PACKET", trace.to_str().unwrap())], args);
    let negotiation = std::fs::read_to_string(&trace)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once("packet:  upload-pack< ").map(|(_, p)| p.to_owned()))
        .filter(|p| !p.starts_with("agent="))
        .collect();
    let srv = root.join("srv/srv.git");
    let refs = String::from_utf8_lossy(&git(stock, &srv, root, &[], &["for-each-ref"]).stdout).into_owned();
    let root_s = root.to_str().unwrap();
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).replace(root_s, "ROOT"),
        stderr: String::from_utf8_lossy(&out.stderr).replace(root_s, "ROOT"),
        refs,
        pack: pack_count(&srv),
        negotiation,
    }
}

/// A stock service program, `git-upload-pack` or `git-receive-pack`, so the server
/// side is stock git for both clients and traces the negotiation.
fn service(stock: &str, name: &str) -> String {
    let out = Command::new(stock).arg("--exec-path").output().unwrap();
    format!("{}/git-{name}", String::from_utf8_lossy(&out.stdout).trim())
}

fn compare(stock: &str, tag: &str, setup: &dyn Fn(&Path, &dyn Fn(&Path, &[&str])), args: &[&str]) -> Run {
    let want = run(stock, stock, &format!("{tag}-s"), setup, args);
    let got = run(stock, ZVCS, &format!("{tag}-z"), setup, args);
    assert_eq!(got, want, "{tag}: zvcs and stock git differ");
    want
}

/// Point the anonymous remote the negotiation child fetches from at stock
/// `upload-pack` (the child is given the URL, so its remote is named by it).
fn traced(stock: &str, url: &str) -> impl Fn(&Path, &dyn Fn(&Path, &[&str])) {
    let key = format!("remote.{url}.uploadpack");
    let program = service(stock, "upload-pack");
    move |root: &Path, ok: &dyn Fn(&Path, &[&str])| ok(&root.join("loc"), &["config", &key, &program])
}

#[test]
fn push_parity_negotiate_leaves_the_common_history_out_of_the_pack() {
    let Some(stock) = stock_git() else { return };
    let setup = traced(stock, "../srv/srv.git");
    let neg = ["-c", "push.negotiate=true"];
    let with = |rest: &[&'static str]| -> Vec<&str> { neg.iter().copied().chain(rest.iter().copied()).collect() };

    // Y's commit, tree and blob: c6 was proved common.
    let run = compare(stock, "save", &setup, &with(&["push", "--no-thin", "origin", "topic"]));
    assert_eq!(run.pack, 3, "{run:?}");
    assert!(run.negotiation.iter().any(|l| l == "wait-for-done"), "{run:?}");

    // Without it the whole history travels.
    let run = compare(stock, "off", &setup, &["push", "--no-thin", "origin", "topic"]);
    assert_eq!(run.pack, 21, "{run:?}");
    assert!(run.negotiation.is_empty(), "{run:?}");

    // A rejected ref's tip and an up-to-date one are negotiation tips too; the
    // deletion is not.
    compare(stock, "mixed", &setup, &with(&["push", "-v", "--no-thin", "origin", "topic", "topic:main", "topic~1:refs/heads/c6"]));
    compare(stock, "delete", &setup, &with(&["push", "origin", ":main"]));
    // A matching push still reaches `send_pack()`, which negotiates for it.
    compare(stock, "matching", &setup, &with(&["push", "origin", ":"]));
    compare(stock, "dry-run", &setup, &with(&["push", "--dry-run", "origin", "topic"]));
    // `git send-pack` negotiates for its refs too.
    let receive_pack = format!("--receive-pack={}", service(stock, "receive-pack"));
    let send_pack = ["-c", "push.negotiate=true", "send-pack", &receive_pack, "../srv/srv.git", "topic"];
    compare(stock, "send-pack", &setup, &send_pack);
}

#[test]
fn push_parity_negotiate_failure_warns_and_pushes_everything() {
    let Some(stock) = stock_git() else { return };
    let setup = traced(stock, "../srv/srv.git");

    // `--negotiate-only requires protocol v2`.
    let run = compare(
        stock,
        "v0",
        &setup,
        &["-c", "push.negotiate=true", "-c", "protocol.version=0", "push", "--no-thin", "origin", "topic"],
    );
    assert!(
        run.stderr.contains("warning: push negotiation failed; proceeding anyway with push\n"),
        "{run:?}"
    );

    // A server that does not advertise `wait-for-done`.
    let no_wait = move |root: &Path, ok: &dyn Fn(&Path, &[&str])| {
        let script = root.join("no-wait-for-done");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n'{}' \"$@\" | perl -e '$|=1; binmode STDIN; binmode STDOUT;\n\
                 while (read(STDIN,$l,4)==4) {{ $n=hex($l); if ($n<4) {{ print $l; next }}\n\
                 read(STDIN,$p,$n-4); $l=sprintf(\"%04x\",length($p)+4) if $p =~ s/ wait-for-done//; print $l,$p }}'\n",
                service(stock, "upload-pack")
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        ok(&root.join("loc"), &["config", "remote.../srv/srv.git.uploadpack", script.to_str().unwrap()]);
    };
    let run = compare(stock, "no-wait", &no_wait, &["-c", "push.negotiate=true", "push", "--no-thin", "origin", "topic"]);
    assert!(run.stderr.starts_with("warning: server does not support wait-for-done\n"), "{run:?}");
    assert_eq!(run.pack, 21, "{run:?}");

    // A bad value dies in `send_pack()`, matching push included. Stock's
    // `receive-pack` child then adds `fatal: the remote end hung up unexpectedly`,
    // which this port's transport has no counterpart for, so the first line, the
    // exit code and the server's refs are what compare.
    let args = ["-c", "push.negotiate=bogus", "push", "origin", ":"];
    let want = crate::run(stock, stock, "bogus-s", &setup, &args);
    let got = crate::run(stock, ZVCS, "bogus-z", &setup, &args);
    assert_eq!(
        (got.code, got.stderr.lines().next(), &got.refs),
        (want.code, want.stderr.lines().next(), &want.refs),
        "{got:?}\n{want:?}"
    );
    assert_eq!(
        want.stderr.lines().next(),
        Some("fatal: bad boolean config value 'bogus' for 'push.negotiate'"),
        "{want:?}"
    );
}

#[test]
fn push_parity_negotiate_reads_the_remotes_negotiation_lists() {
    // `remote.<name>.negotiationRestrict` is git 2.56's name for the key.
    let Some(stock) = stock_git_at_least((2, 56, 0)) else { return };
    let base = traced(stock, "../srv/srv.git");
    let with_lists = move |root: &Path, ok: &dyn Fn(&Path, &[&str])| {
        base(root, ok);
        let loc = root.join("loc");
        ok(&loc, &["config", "remote.origin.negotiationRestrict", "origin/main~2"]);
        ok(&loc, &["config", "remote.origin.negotiationInclude", "origin/main~4"]);
    };
    let neg = ["-c", "push.negotiate=true"];
    let run = compare(stock, "lists", &with_lists, &[neg[0], neg[1], "push", "--no-thin", "origin", "topic"]);
    assert_eq!(run.negotiation.iter().filter(|l| l.starts_with("have ")).count(), 3, "{run:?}");
    // `git send-pack` leaves both lists unset even when told the remote's name.
    let receive_pack = format!("--receive-pack={}", service(stock, "receive-pack"));
    let send_pack = [neg[0], neg[1], "send-pack", &receive_pack, "--remote=origin", "../srv/srv.git", "topic"];
    compare(stock, "lists-send-pack", &with_lists, &send_pack);
}

#[test]
fn push_parity_negotiate_over_file_url() {
    let Some(stock) = stock_git() else { return };
    let file_url = |root: &Path, ok: &dyn Fn(&Path, &[&str])| {
        let url = format!("file://{}/srv/srv.git", root.display());
        let loc = root.join("loc");
        ok(&loc, &["remote", "set-url", "origin", &url]);
        ok(&loc, &["config", &format!("remote.{url}.uploadpack"), &service(stock, "upload-pack")]);
    };
    let run = compare(stock, "file", &file_url, &["-c", "push.negotiate=true", "push", "--no-thin", "origin", "topic"]);
    assert_eq!(run.pack, 3, "{run:?}");
    let run = compare(
        stock,
        "file-v0",
        &file_url,
        &["-c", "push.negotiate=true", "-c", "protocol.version=0", "push", "--no-thin", "origin", "topic"],
    );
    assert_eq!(run.pack, 21, "{run:?}");
}

#[test]
fn push_parity_negotiate_over_smart_http() {
    let Some(stock) = stock_git() else { return };
    for protocol in ["2", "0"] {
        let mut runs = Vec::new();
        for (bin, side) in [(stock, "s"), (ZVCS, "z")] {
            let fx = fixture(stock, &format!("http-v{protocol}-{side}"));
            let (port, posts) = cgi_server::spawn(stock, fx.root.join("srv"), cgi_server::Fault::NONE);
            let url = format!("http://127.0.0.1:{port}/srv.git");
            let protocol_cfg = format!("protocol.version={protocol}");
            let out = git(
                bin,
                &fx.root.join("loc"),
                &fx.root,
                &[],
                &["-c", "push.negotiate=true", "-c", &protocol_cfg, "push", "--no-thin", &url, "topic"],
            );
            let srv = fx.root.join("srv/srv.git");
            let host = format!("127.0.0.1:{port}");
            runs.push((
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).replace(&host, "HOST"),
                String::from_utf8_lossy(&out.stderr).replace(&host, "HOST"),
                String::from_utf8_lossy(&git(stock, &srv, &fx.root, &[], &["for-each-ref"]).stdout).into_owned(),
                pack_count(&srv),
                posts.load(std::sync::atomic::Ordering::SeqCst),
            ));
        }
        // `git-remote-curl` has no `thin` option, so stock pushes a thin pack over
        // HTTP whatever `--no-thin` says, and the server's `index-pack --fix-thin`
        // appends the bases it names; this port never sends a thin pack. So the
        // kept packs are checked one by one rather than against each other.
        let (pack_s, pack_z) = (runs[0].4, runs[1].4);
        runs.iter_mut().for_each(|r| r.4 = 0);
        assert_eq!(runs[1], runs[0], "protocol {protocol}: zvcs and stock git differ");
        if protocol == "2" {
            assert_eq!(pack_z, 3, "protocol 2: only what negotiation left out");
            assert!(pack_s < 21, "protocol 2: stock negotiated too: {pack_s}");
        } else {
            assert_eq!((pack_z, pack_s), (21, 21), "protocol 0: everything");
        }
    }
}
