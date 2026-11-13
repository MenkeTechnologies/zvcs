//! A server that dies while it reads the request.
//!
//! git builds a request whole in a `struct strbuf req_buf` and writes it with one
//! `write_in_full()`: `find_common()` through `send_request()` (fetch-pack.c:254-264, :361-459),
//! `send_fetch_request()` for a v2 `fetch` (fetch-pack.c:1384-1445), `send_pack()` for its
//! commands (send-pack.c:486-694). A server that dies over the first line it reads has then been
//! handed all of it, so the client finds out from the read that follows, whose end of stream is
//! `die(_("the remote end hung up unexpectedly"))` (pkt-line.c:364-372): `find_common()` reads its
//! shallow lines and ACK/NAKs without `PACKET_READ_GENTLE_ON_EOF` (fetch-pack.c:372-374).
//!
//! zvcs wrote a request one packet line per `write()`. `upload-pack` dying over `deepen-not  1`
//! closed the pipe under the lines still to come, and the `EPIPE` reached the top-level handler,
//! which takes a broken pipe for a closed stdout and raises `SIGPIPE`: the CI run printed the
//! server's line and nothing else, killed by the signal. Over v0/v1 it also read an end of stream
//! among the acknowledgements as a NAK and went on to a pack that never came:
//! `fetch-pack: unexpected disconnect while reading sideband packet` and
//! `fatal: protocol error: bad pack header`.
//!
//! The server is stock git's `upload-pack`/`receive-pack` behind an `--upload-pack`/
//! `--receive-pack` proxy that forwards the advertisement and the client's first `$DIE_AFTER`
//! packet lines, then kills the server, reads the rest of the client's request through its
//! flush packet, or the `done` that ends a v0/v1 negotiation without one, and exits 128 with
//! both pipes closed. Reading the request whole keeps every write of it off a closed pipe:
//! stock git writes `ls-refs` one `packet_write_fmt()` per line
//! (connect.c:583-606) and `send_pack()`'s closing flush on its own (send-pack.c:693-694), and
//! one of those writes meeting a proxy already gone is death by `SIGPIPE`, not a hang-up.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::path::Path;
use std::process::Command;

const PROXY: &str = r#"#!/usr/bin/perl
use strict; use IPC::Open2; use IO::Select;
my $pid = open2(my $sout, my $sin, $ENV{SERVER_GIT}, $ENV{SERVICE}, $ARGV[-1]);
binmode $_ for $sout, $sin, \*STDIN, \*STDOUT;
my $sel = IO::Select->new($sout, \*STDIN);
my ($buf, $seen, $dead) = ('', 0, 0);
while (1) {
  for my $fh ($sel->can_read) {
    my $d; my $r = sysread($fh, $d, 65536);
    if ($fh == $sout) { exit 0 unless $r; syswrite(STDOUT, $d); next; }
    exit 128 unless $r;
    $buf .= $d;
    while (length($buf) >= 4) {
      my $l = hex(substr($buf, 0, 4)); my $flush = $l == 0; $l = 4 if $l < 4;
      last if length($buf) < $l;
      my $end = $flush || substr($buf, 4, $l - 4) eq "done\n";
      syswrite($sin, substr($buf, 0, $l)) unless $dead;
      $buf = substr($buf, $l);
      if (!$dead && ++$seen >= $ENV{DIE_AFTER}) {
        kill 'KILL', $pid; $sel->remove($sout); $dead = 1;
      }
      if ($dead && $end) {
        print STDERR "fatal: server died after $seen\n";
        close STDIN; close STDOUT; exit 128;
      }
    }
  }
}
"#;

/// Run stock git in `dir` for fixture setup, both sides being built by the same binary.
fn setup(stock: &str, dir: &Path, args: &[&str]) {
    let out = Command::new(stock)
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

/// Under each side's root: `remote.git` one commit ahead of `behind`, which has a commit of its
/// own, an empty `empty` with `remote.git` as `origin`, and the proxy.
fn fixture(stock: &str, side: &twin_repo::Side) {
    let root = side.root.as_path();
    let mc = ["-c", "maintenance.auto=false"];
    setup(stock, root, &[&mc[..], &["clone", "-q", "repo", "behind"]].concat());
    setup(stock, &root.join("behind"), &["commit", "-q", "--allow-empty", "-m", "local"]);
    setup(stock, &side.repo(), &["commit", "-q", "--allow-empty", "-m", "four"]);
    setup(stock, root, &[&mc[..], &["clone", "-q", "--bare", "repo", "remote.git"]].concat());
    setup(stock, &root.join("behind"), &["remote", "set-url", "origin", "../remote.git"]);
    setup(stock, root, &["init", "-q", "-b", "main", "empty"]);
    setup(stock, &root.join("empty"), &["remote", "add", "origin", "../remote.git"]);
    let proxy = root.join("proxy.pl");
    std::fs::write(&proxy, PROXY).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&proxy, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// `git -c protocol.version=<v> <args>` in a fresh copy of `<root>/<repo>`, through the proxy.
fn run(stock: &str, side: &twin_repo::Side, repo: &str, v: &str, die_after: &str, args: &[&str]) -> twin_repo::Out {
    let copy = side.root.join("copy");
    let _ = std::fs::remove_dir_all(&copy);
    let cp = Command::new("cp").arg("-R").arg(side.root.join(repo)).arg(&copy).status().unwrap();
    assert!(cp.success());
    let proto = format!("protocol.version={v}");
    let all = [&["-c", proto.as_str(), "-c", "maintenance.auto=false"][..], args].concat();
    let service = if args[0] == "push" || args[0] == "send-pack" { "receive-pack" } else { "upload-pack" };
    side.run_in(&copy, &[("SERVER_GIT", stock), ("SERVICE", service), ("DIE_AFTER", die_after)], &all)
}

#[test]
fn a_server_dying_over_the_request_is_a_hang_up() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("fetch-server-dies-over-request", stock);
    for side in [&s, &z] {
        fixture(stock, side);
    }
    let proxy = format!("--upload-pack={}", s.root.join("proxy.pl").display());
    let zproxy = format!("--upload-pack={}", z.root.join("proxy.pl").display());
    // upload-pack itself dies over `deepen-not  1`, the line ahead of the wants (the proxy only
    // passes everything on); every version, several times over, as the CI failure was a race.
    for v in ["0", "1", "2"] {
        for value in [" 1", "nosuch"] {
            let flag = format!("--shallow-exclude={value}");
            for _ in 0..3 {
                let want = run(stock, &s, "empty", v, "1000", &["fetch", flag.as_str(), proxy.as_str(), "origin"]);
                assert_eq!(want.code, 128, "v{v} {value:?}: {want:?}");
                assert!(want.stderr.ends_with("fatal: the remote end hung up unexpectedly\n"), "{want:?}");
                let got = run(stock, &z, "empty", v, "1000", &["fetch", flag.as_str(), zproxy.as_str(), "origin"]);
                assert_eq!(got, want, "v{v} {value:?}");
            }
        }
    }
    // The proxy cuts the v0/v1 request after its first lines: in the wants, at the flush, in
    // the haves of a repository that has some, and with `--depth`, ahead of the shallow lines.
    for v in ["0", "1"] {
        for repo in ["empty", "behind"] {
            for die_after in ["1", "2", "3"] {
                for extra in [&[][..], &["--depth=1"][..]] {
                    let mut args = vec!["fetch"];
                    args.extend_from_slice(extra);
                    let want = run(stock, &s, repo, v, die_after, &[&args[..], &[proxy.as_str(), "origin"]].concat());
                    assert_eq!(want.code, 128, "v{v} {repo} {die_after} {extra:?}: {want:?}");
                    assert!(want.stderr.ends_with("fatal: the remote end hung up unexpectedly\n"), "{want:?}");
                    let got = run(stock, &z, repo, v, die_after, &[&args[..], &[zproxy.as_str(), "origin"]].concat());
                    assert_eq!(got, want, "v{v} {repo} {die_after} {extra:?}");
                }
            }
        }
    }
    // v2: in the ls-refs request, and in the fetch request after it.
    for repo in ["empty", "behind"] {
        for die_after in ["4", "13"] {
            let want = run(stock, &s, repo, "2", die_after, &["fetch", proxy.as_str(), "origin"]);
            assert_eq!(want.code, 128, "{repo} {die_after}: {want:?}");
            let got = run(stock, &z, repo, "2", die_after, &["fetch", zproxy.as_str(), "origin"]);
            assert_eq!(got, want, "{repo} {die_after}");
        }
    }
    // A clone, whose request carries only wants.
    for v in ["0", "2"] {
        let want = run(stock, &s, "empty", v, "1", &["clone", "-q", proxy.as_str(), "../remote.git", "c"]);
        let got = run(stock, &z, "empty", v, "1", &["clone", "-q", zproxy.as_str(), "../remote.git", "c"]);
        assert_eq!(got, want, "v{v}");
    }
}

#[test]
fn a_receive_pack_dying_over_the_commands_is_a_hang_up() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("push-server-dies-over-request", stock);
    for side in [&s, &z] {
        fixture(stock, side);
    }
    let proxy = format!("--receive-pack={}", s.root.join("proxy.pl").display());
    let zproxy = format!("--receive-pack={}", z.root.join("proxy.pl").display());
    for v in ["0", "2"] {
        for die_after in ["1", "2", "3"] {
            for verb in ["push", "send-pack"] {
                let refs = ["main:refs/heads/new", "main:refs/heads/new2"];
                let want = run(stock, &s, "behind", v, die_after, &[&[verb, proxy.as_str(), "../remote.git"][..], &refs].concat());
                assert_eq!(want.code, 128, "v{v} {die_after} {verb}: {want:?}");
                let got = run(stock, &z, "behind", v, die_after, &[&[verb, zproxy.as_str(), "../remote.git"][..], &refs].concat());
                assert_eq!(got, want, "v{v} {die_after} {verb}");
            }
        }
    }
}
