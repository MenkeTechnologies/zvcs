//! Over protocol v0 and v1, a server that closes the pipe before the client writes to it.
//!
//! What a write into the closed pipe does depends on the `SIGPIPE` disposition in force:
//!
//! * `fetch` ignores `SIGPIPE` around `do_fetch()` (builtin/fetch.c:2494-2498), so
//!   `send_request()`'s `write()` of the wants returns `EPIPE` and dies
//!   `unable to write to remote: Broken pipe` (fetch-pack.c:254-264), exit 128.
//! * `clone` and `fetch-pack` leave it at its default (common-init.c:27-35): the same `write()`
//!   kills them, and `clone`'s `remove_junk_on_signal()` takes the new directory down first
//!   (builtin/clone.c:416-421).
//! * With nothing to fetch, the only write is `disconnect_git()`'s closing flush
//!   (transport.c:1020-1030). `ls-remote` writes it after the listing, so the refs `printf()`
//!   left in a fully buffered stdout die with it; a local `clone` writes it after `clone_local()`
//!   has said `done.` (builtin/clone.c:1597-1619). `fetch` writes it after `sigchain_pop(SIGPIPE)`,
//!   which restores `SIG_IGN` when `do_fetch()` took the process's first lock, as
//!   `activate_tempfile()` pushed its handler on top of the ignore (tempfile.c:112-120): a
//!   refspec-less fetch locks `refs/remotes/origin/HEAD` in `set_head()` and dies
//!   `unable to write flush packet: Broken pipe` (pkt-line.c:93-98), while `fetch origin main`
//!   takes no lock and is killed by the signal.
//!
//! The proxy forwards the advertisement up to its flush, then closes its stdin before it hands
//! the client that flush, so the client cannot write before the pipe is closed. It is named
//! `exec <proxy>` so that no `sh -c` shell holds the request pipe open (see
//! push_parity_request_into_closed_pipe.rs).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::path::Path;
use std::process::Command;

const PROXY: &str = r#"#!/usr/bin/perl
use strict; use IPC::Open2;
my $pid = open2(my $sout, my $sin, $ENV{SERVER_GIT}, 'upload-pack', $ARGV[-1]);
binmode $_ for $sout, \*STDOUT;
my $buf = '';
while (sysread($sout, my $d, 65536)) {
  $buf .= $d;
  while (length($buf) >= 4) {
    my $l = hex(substr($buf, 0, 4));
    if ($l == 0) {
      kill 'KILL', $pid; close STDIN; syswrite(STDOUT, $d); close STDOUT; exit 128;
    }
    last if length($buf) < $l;
    $buf = substr($buf, $l);
  }
  syswrite(STDOUT, $d);
}
exit 0;
"#;

/// Run stock git in `dir` for fixture setup, both sides being built by the same binary.
fn setup(stock: &str, dir: &Path, args: &[&str]) {
    let out = Command::new(stock)
        .args(["-c", "maintenance.auto=false"])
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000100 +0000")
        .env("GIT_COMMITTER_DATE", "1700000100 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// The stock and the zvcs side, each root holding `remote.git`, a bare repository of two commits;
/// `empty`, with `remote.git` as `origin`; `uptodate`, a clone of `remote.git`; and the proxy.
/// Built once by stock git and copied, with `origin` relative in both repositories.
fn sides(stock: &str) -> (twin_repo::Side, twin_repo::Side) {
    let base = std::env::temp_dir().join(format!("zvcs-v0-request-into-closed-pipe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("stock")).unwrap();
    let root = std::fs::canonicalize(base.join("stock")).unwrap();
    setup(stock, &root, &["init", "-q", "-b", "main", "repo"]);
    for msg in ["one", "two"] {
        setup(stock, &root.join("repo"), &["commit", "-q", "--allow-empty", "-m", msg]);
    }
    setup(stock, &root, &["clone", "-q", "--bare", "repo", "remote.git"]);
    setup(stock, &root, &["clone", "-q", "remote.git", "uptodate"]);
    setup(stock, &root.join("uptodate"), &["remote", "set-url", "origin", "../remote.git"]);
    setup(stock, &root, &["init", "-q", "-b", "main", "empty"]);
    setup(stock, &root.join("empty"), &["remote", "add", "origin", "../remote.git"]);
    let proxy = root.join("proxy.pl");
    std::fs::write(&proxy, PROXY).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&proxy, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cp = Command::new("cp").arg("-R").arg(&root).arg(base.join("zvcs")).status().unwrap();
    assert!(cp.success());
    let zroot = std::fs::canonicalize(base.join("zvcs")).unwrap();
    (
        twin_repo::Side { root, bin: stock.to_owned() },
        twin_repo::Side { root: zroot, bin: twin_repo::ZVCS.to_owned() },
    )
}

/// `git -c protocol.version=<v> <args>` in a fresh copy of `<root>/<repo>`, `<proxy>` standing for
/// the proxy's path, and whether the clone destination `c` is left behind.
fn run(stock: &str, side: &twin_repo::Side, repo: &str, v: &str, args: &[&str]) -> (twin_repo::Out, bool) {
    let copy = side.root.join("copy");
    let _ = std::fs::remove_dir_all(&copy);
    let cp = Command::new("cp").arg("-R").arg(side.root.join(repo)).arg(&copy).status().unwrap();
    assert!(cp.success());
    let proxy = side.root.join("proxy.pl").display().to_string();
    let args: Vec<String> = args.iter().map(|a| a.replace("<proxy>", &proxy)).collect();
    let proto = format!("protocol.version={v}");
    let mut all = vec!["-c", proto.as_str(), "-c", "maintenance.auto=false"];
    all.extend(args.iter().map(String::as_str));
    let out = side.run_in(&copy, &[("SERVER_GIT", stock)], &all);
    (out, copy.join("c").exists())
}

#[test]
fn a_v0_request_into_a_closed_pipe_ends_as_git_ends() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = sides(stock);
    let up = "--upload-pack=exec <proxy>";
    // The repository, the protocol versions, the command, and what git does: the exit (-1 for
    // the signal), stderr when it says something, and whether the clone directory survives.
    let killed = (-1, None, false);
    let cases: [(&str, &[&str], &[&str], (i32, Option<&str>, bool)); 6] = [
        ("empty", &["0", "1"], &["fetch", up, "origin"], (128, Some("fatal: unable to write to remote: Broken pipe\n"), false)),
        ("empty", &["0", "1"], &["clone", up, "../remote.git", "c"], (-1, Some("Cloning into 'c'...\ndone.\n"), false)),
        ("empty", &["0", "1"], &["ls-remote", up, "origin"], killed),
        ("empty", &["0"], &["fetch-pack", up, "../remote.git", "refs/heads/main"], killed),
        ("uptodate", &["0"], &["fetch", up, "origin"], (128, Some("fatal: unable to write flush packet: Broken pipe\n"), false)),
        ("uptodate", &["0"], &["fetch", up, "origin", "main"], killed),
    ];
    for (repo, versions, args, (code, stderr, kept)) in cases {
        for v in versions {
            let want = run(stock, &s, repo, v, args);
            let got = run(stock, &z, repo, v, args);
            assert_eq!(got, want, "v{v} {repo} {args:?}");
            // So the comparison cannot pass on a proxy that let the request through.
            assert_eq!((want.0.code, want.1), (code, kept), "v{v} {repo} {args:?}: {want:?}");
            if let Some(stderr) = stderr {
                assert_eq!(want.0.stderr, stderr, "v{v} {repo} {args:?}");
            }
            // `ls-remote`'s listing sat in stdio's buffer when the signal came.
            assert_eq!(want.0.stdout, "", "v{v} {repo} {args:?}");
        }
    }
}
