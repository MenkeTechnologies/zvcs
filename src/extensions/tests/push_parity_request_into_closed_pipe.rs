//! A request written into a pipe the server has already closed.
//!
//! git leaves `SIGPIPE` at its default (common-init.c:27-35), and what a closed pipe does to
//! it depends on who was writing:
//!
//! * `send_pack()` writes its commands with `write_or_die()` and `packet_flush()`
//!   (send-pack.c:693-694): the `write()` raises `SIGPIPE`, or `check_pipe()` does
//!   (write-or-die.c:91-97, run-command.c:1150-1161), and push dies by the signal with nothing
//!   on stderr.
//! * The pack is `pack-objects` writing to the same descriptor (send-pack.c:81-84). The signal
//!   kills that child, which `odb_pack_generator_files_finish()` passes over in silence
//!   (odb/source-files.c:777-790), and `send_pack()` reads on (send-pack.c:712-735) into the end
//!   of the stream: `send-pack: unexpected disconnect while reading sideband packet` and
//!   `fatal: the remote end hung up unexpectedly`, exit 128.
//! * `get_remote_refs()` writes `ls-refs` one `packet_write_fmt()` per line (connect.c:583-606),
//!   whose `check_pipe()` raises `SIGPIPE` even under `fetch`'s `SIG_IGN` (pkt-line.c:171-174).
//!
//! zvcs printed `fatal: Broken pipe (os error 32)` for a send-pack whose commands or pack met the
//! closed pipe, died by the signal where push's pack met it, and reported a closed pipe under
//! `ls-refs` as `zvcs: clone: Broken pipe (os error 32)`.
//!
//! The proxy closes its stdin before the client can write into it, so each case is one timing:
//! `cmd` closes it ahead of the last of the advertisement, before the client has read what it
//! needs to answer; `pack` closes it once the commands' flush has come in, under a pack too big
//! for the pipe to have taken whole.
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
my ($obuf, $ibuf) = ('', '');
sub cut { kill 'KILL', $pid; close STDIN; syswrite(STDOUT, $_[0]) if defined $_[0]; close STDOUT; exit 128; }
while (1) {
  for my $fh ($sel->can_read) {
    my $d; my $r = sysread($fh, $d, 65536);
    if ($fh == $sout) {
      exit 0 unless $r;
      $obuf .= $d;
      while ($ENV{MODE} eq 'cmd' && length($obuf) >= 4) {
        my $l = hex(substr($obuf, 0, 4));
        cut($d) if $l == 0;
        last if length($obuf) < $l;
        $obuf = substr($obuf, $l);
      }
      syswrite(STDOUT, $d);
      next;
    }
    exit 128 unless $r;
    $ibuf .= $d;
    while (length($ibuf) >= 4) {
      my $l = hex(substr($ibuf, 0, 4)); my $flush = $l == 0; $l = 4 if $l < 4;
      last if length($ibuf) < $l;
      syswrite($sin, substr($ibuf, 0, $l)); $ibuf = substr($ibuf, $l);
      cut() if $flush && $ENV{MODE} eq 'pack';
    }
  }
}
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

/// Under each side's root: `remote.git`, a bare copy of `repo`; `big`, a clone of it one commit
/// ahead with 512 KiB that do not compress; `empty`, with `remote.git` as `origin`; the proxy.
fn fixture(stock: &str, side: &twin_repo::Side) {
    let root = side.root.as_path();
    setup(stock, root, &["clone", "-q", "--bare", "repo", "remote.git"]);
    setup(stock, root, &["clone", "-q", "remote.git", "big"]);
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let noise: Vec<u8> = (0..512 * 1024)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    std::fs::write(root.join("big/noise"), noise).unwrap();
    setup(stock, &root.join("big"), &["add", "noise"]);
    setup(stock, &root.join("big"), &["commit", "-q", "-m", "noise"]);
    setup(stock, root, &["init", "-q", "-b", "main", "empty"]);
    setup(stock, &root.join("empty"), &["remote", "add", "origin", "../remote.git"]);
    let proxy = root.join("proxy.pl");
    std::fs::write(&proxy, PROXY).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&proxy, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// `git <args>` in `<root>/<repo>` through the proxy in `mode`, `<proxy>` standing for its path.
fn run(stock: &str, side: &twin_repo::Side, repo: &str, mode: &str, args: &[&str]) -> twin_repo::Out {
    let proxy = side.root.join("proxy.pl").display().to_string();
    let args: Vec<String> = args.iter().map(|a| a.replace("<proxy>", &proxy)).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let service = if args[0] == "push" || args[0] == "send-pack" { "receive-pack" } else { "upload-pack" };
    let env = [("SERVER_GIT", stock), ("SERVICE", service), ("MODE", mode)];
    let all = [&["-c", "maintenance.auto=false"][..], &args].concat();
    side.run_in(&side.root.join(repo), &env, &all)
}

#[test]
fn a_request_into_a_closed_pipe_ends_as_git_ends() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("request-into-closed-pipe", stock);
    for side in [&s, &z] {
        fixture(stock, side);
    }
    let push = |verb: &'static str| [verb, "--receive-pack=<proxy>", "../remote.git", "main:refs/heads/new"];
    let cases: [(&str, &str, Vec<&str>); 7] = [
        ("big", "cmd", push("push").to_vec()),
        ("big", "cmd", push("send-pack").to_vec()),
        ("big", "pack", push("push").to_vec()),
        ("big", "pack", push("send-pack").to_vec()),
        ("empty", "cmd", vec!["-c", "protocol.version=2", "clone", "-q", "--upload-pack=<proxy>", "../remote.git", "c"]),
        ("empty", "cmd", vec!["-c", "protocol.version=2", "fetch", "--upload-pack=<proxy>", "origin"]),
        ("empty", "cmd", vec!["-c", "protocol.version=2", "ls-remote", "--upload-pack=<proxy>", "origin"]),
    ];
    for (repo, mode, args) in &cases {
        let want = run(stock, &s, repo, mode, args);
        let got = run(stock, &z, repo, mode, args);
        assert_eq!(got, want, "{mode} {args:?}");
    }
    // What git does in each timing, so the comparison above cannot pass on a proxy that missed it.
    let pack = run(stock, &s, "big", "pack", &push("send-pack"));
    assert_eq!(pack.code, 128, "{pack:?}");
    assert!(pack.stderr.ends_with("fatal: the remote end hung up unexpectedly\n"), "{pack:?}");
    let cmd = run(stock, &s, "big", "cmd", &push("send-pack"));
    assert_eq!((cmd.code, cmd.stderr.as_str()), (-1, ""), "{cmd:?}");
}
