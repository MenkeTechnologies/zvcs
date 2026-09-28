//! A pushed ref the server never reported on is `[remote failure]`.
//!
//! After `receive_status()` a command the report never mentioned keeps
//! `REF_STATUS_EXPECTING_REPORT`, and `print_one_push_report()` prints it as
//! `print_ref_status('!', "[remote failure]", …, "remote failed to report
//! status", …)` (transport.c:793-798) — in the porcelain form too. zvcs knew
//! the status (`send-pack` printed it right) but `push` labelled it
//! `[rejected]`, as if this side had refused the update.
//!
//! The server is a `--receive-pack` proxy around this binary's own
//! `receive-pack` that turns side-band off and swallows the `ok` line for
//! `refs/heads/topic`.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const PROXY: &str = r#"#!/usr/bin/perl
use strict; $| = 1;
open(my $srv, '-|', $ENV{SERVER_GIT}, 'receive-pack', $ARGV[0]) or die;
binmode $srv; binmode STDOUT;
my $first = 1;
while (1) {
  my $len; last unless read($srv, $len, 4) == 4;
  my $n = hex($len);
  if ($n == 0) { print "0000"; next; }
  my $data; read($srv, $data, $n - 4);
  if ($first) { $data =~ s/ side-band-64k| side-band//g; $first = 0; }
  next if $data eq "ok refs/heads/topic\n";
  printf "%04x%s", length($data) + 4, $data;
}
"#;

struct Fixture {
    root: PathBuf,
    work: PathBuf,
    proxy: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// `main` and `topic`, cloned bare to `dst.git`; both then advanced a commit.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-remote-failure-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let proxy = root.join("proxy.pl");
        std::fs::write(&proxy, PROXY).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&proxy, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let f = Fixture { root, work, proxy };
        f.run(&["init", "-q", "-b", "main", "."]);
        f.run(&["commit", "-q", "--allow-empty", "-m", "one"]);
        f.run(&["branch", "topic"]);
        f.run(&["clone", "-q", "--bare", ".", "../dst.git"]);
        f.run(&["commit", "-q", "--allow-empty", "-m", "two"]);
        f.run(&["branch", "-f", "topic"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("SERVER_GIT", BIN)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn receive_pack(&self) -> String {
        format!("--receive-pack={}", self.proxy.display())
    }

    fn short(&self, rev: &str) -> String {
        self.run(&["rev-parse", "--short", rev]).0.trim_end().to_owned()
    }
}

#[test]
fn an_unanswered_update_is_a_remote_failure() {
    let f = Fixture::new("human");
    let rp = f.receive_pack();
    let range = format!("{}..{}", f.short("main~1"), f.short("main"));
    assert_eq!(
        f.run(&["push", &rp, "../dst.git", "main", "topic"]),
        (
            String::new(),
            format!(
                "To ../dst.git\n   {range}  main -> main\n \
                 ! [remote failure]  topic -> topic (remote failed to report status)\n\
                 error: failed to push some refs to '../dst.git'\n"
            ),
            1
        )
    );
}

#[test]
fn an_unanswered_deletion_is_a_remote_failure_in_porcelain() {
    let f = Fixture::new("porcelain");
    let rp = f.receive_pack();
    assert_eq!(
        f.run(&["push", "--porcelain", "--delete", &rp, "../dst.git", "topic"]),
        (
            "To ../dst.git\n\
             !\t:refs/heads/topic\t[remote failure] (remote failed to report status)\n\
             Done\n"
                .into(),
            "error: failed to push some refs to '../dst.git'\n".into(),
            1
        )
    );
}
