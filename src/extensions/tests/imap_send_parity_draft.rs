//! `git imap-send --draft`, new in git 2.56.
//!
//! imap-send.c:1422-1423 (2.56.0): `APPEND "<box>" (\Draft) {<n>}` under
//! `--draft`, `APPEND "<box>" {<n>}` otherwise. The wire is observed through
//! `imap.tunnel`, which both stock and this port drive with the in-tree client;
//! the tunnel is a small IMAP responder that logs every command line it reads.
//! The logs and messages below were measured against stock git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A pre-authenticated IMAP server on stdin/stdout: every tagged command is
/// answered `OK`, a synchronising literal gets its `+` continuation, and each
/// command line is appended to `$FAKEIMAP_LOG`.
const SERVER: &str = r#"#!/usr/bin/perl
use strict; $| = 1;
open my $L, '>>', $ENV{FAKEIMAP_LOG} or die;
select((select($L), $| = 1)[0]);
print "* PREAUTH ready\r\n";
while (my $line = <STDIN>) {
    print $L "C: $line";
    my ($tag) = $line =~ /^(\S+)/;
    if ($line =~ /\{(\d+)\}\r?\n$/) {
        print "+ go\r\n";
        my $buf = '';
        read(STDIN, $buf, $1);
        my $rest = <STDIN>;
        print $L "L: $1 bytes\n";
        print "$tag OK done\r\n";
        next;
    }
    if ($line =~ /LOGOUT/) { print "* BYE\r\n$tag OK bye\r\n"; last; }
    print "$tag OK done\r\n";
}
"#;

const MBOX: &str = "From 0000 Mon Sep 17 00:00:00 2001\nFrom: a <a@x>\nDate: Mon, 1 Jan 2024 00:00:00 +0000\nSubject: hi\n\nbody\n";

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zvcs-imap-draft-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let server = dir.join("server.pl");
    std::fs::write(&server, SERVER).unwrap();
    dir
}

/// Run `git -c imap.tunnel=... -c imap.folder=Drafts imap-send <args>` with
/// [`MBOX`] on stdin; returns the exit code and the server's log.
fn send(dir: &Path, args: &[&str]) -> (i32, String) {
    let log = dir.join("log");
    let _ = std::fs::remove_file(&log);
    let tunnel = format!("imap.tunnel=perl {}", dir.join("server.pl").display());
    let mut child = Command::new(BIN)
        .args(["-c", &tunnel, "-c", "imap.folder=Drafts", "imap-send"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("FAKEIMAP_LOG", &log)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(MBOX.as_bytes()).unwrap();
    let code = child.wait().unwrap().code().expect("no signal");
    (code, std::fs::read_to_string(&log).unwrap_or_default())
}

fn wire(append: &str) -> String {
    format!("C: 1 CAPABILITY\r\nC: 2 EXAMINE \"Drafts\"\r\nC: 3 {append}\r\nL: 74 bytes\nC: 4 LOGOUT\r\n")
}

#[test]
fn draft_flags_the_append() {
    let dir = fixture("on");
    assert_eq!(send(&dir, &["--draft"]), (0, wire("APPEND \"Drafts\" (\\Draft) {74}")));
    // Unique-prefix abbreviation reaches the same option.
    assert_eq!(send(&dir, &["--dra"]), (0, wire("APPEND \"Drafts\" (\\Draft) {74}")));
}

#[test]
fn without_draft_the_append_is_unflagged() {
    let dir = fixture("off");
    assert_eq!(send(&dir, &[]), (0, wire("APPEND \"Drafts\" {74}")));
    assert_eq!(send(&dir, &["--no-draft"]), (0, wire("APPEND \"Drafts\" {74}")));
    assert_eq!(send(&dir, &["--draft", "--no-draft"]), (0, wire("APPEND \"Drafts\" {74}")));
}

#[test]
fn draft_takes_no_value_and_is_in_the_usage() {
    let dir = fixture("usage");
    let out = Command::new(BIN)
        .args(["imap-send", "--draft=1"])
        .current_dir(&dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(129));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "error: option `draft' takes no value\n");

    let out = Command::new(BIN).args(["imap-send", "-h"]).current_dir(&dir).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "usage: git imap-send [-v] [-q] [--[no-]curl] [--[no-]draft] [(--folder|-f) <folder>] < <mbox>\n   \
         or: git imap-send --list\n\
         \n    \
         -v, --[no-]verbose    be more verbose\n    \
         -q, --[no-]quiet      be more quiet\n    \
         --[no-]curl           use libcurl to communicate with the IMAP server\n    \
         --[no-]draft          mark uploaded messages with the IMAP \\Draft flag\n    \
         -f, --[no-]folder <folder>\n                          \
         specify the IMAP folder\n    \
         --[no-]list           list all folders on the IMAP server\n\n"
    );
}
