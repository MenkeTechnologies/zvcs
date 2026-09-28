//! An upload-pack advertisement the other end cut short.
//!
//! `discover_version()` peeks the first packet line and, on
//! `PACKET_READ_EOF`, calls `die_initial_contact(0)` — the fixed `Could not
//! read from remote repository.` block (connect.c:61-74, 151-153). A protocol
//! v2 capability list that ends before its flush is
//! `die(_("expected flush after capabilities"))` in
//! `process_capabilities_v2()` (connect.c:134-140). Both exit 128, from
//! `ls-remote`, `fetch` and `clone` alike. zvcs reported the vendored
//! transport's `An IO error occurred when talking to the server` (and, from
//! `fetch`/`clone`, a `zvcs:`-prefixed error at exit 1).
//!
//! The server is an `--upload-pack` proxy around this binary's own
//! `upload-pack` that forwards the first `$CUT` packet lines and exits.
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

const PROXY: &str = r#"#!/usr/bin/perl
use strict; $| = 1;
open(my $srv, '-|', $ENV{SERVER_GIT}, 'upload-pack', $ARGV[0]) or die;
binmode $srv; binmode STDOUT;
for (1..$ENV{CUT}) {
  my $len; last unless read($srv, $len, 4) == 4;
  my $n = hex($len);
  if ($n == 0) { print "0000"; next; }
  my $data; read($srv, $data, $n - 4);
  printf "%04x%s", length($data) + 4, $data;
}
use POSIX (); POSIX::_exit(0);
"#;

const NO_CONTACT: &str = "fatal: Could not read from remote repository.\n\n\
Please make sure you have the correct access rights\nand the repository exists.\n";

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
    /// A one-commit repository `up` next to an empty one `work`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-advertisement-hang-up-{tag}-{}", std::process::id()));
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
        f.run("0", &["init", "-q", "-b", "main", "../up"]);
        f.run("0", &["-C", "../up", "commit", "-q", "--allow-empty", "-m", "one"]);
        f.run("0", &["init", "-q", "-b", "main", "."]);
        f
    }

    fn run(&self, cut: &str, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("CUT", cut)
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
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    fn upload_pack(&self) -> String {
        format!("--upload-pack={}", self.proxy.display())
    }
}

#[test]
fn a_silent_server_is_the_initial_contact_block() {
    let f = Fixture::new("silent");
    let up = f.upload_pack();
    let expect = (String::new(), NO_CONTACT.to_owned(), 128);
    assert_eq!(f.run("0", &["ls-remote", &up, "../up"]), expect);
    assert_eq!(f.run("0", &["fetch", &up, "../up"]), expect);
    assert_eq!(f.run("0", &["clone", "-q", &up, "../up", "c"]), expect);
    assert!(!f.work.join("c").exists());
}

#[test]
fn a_v2_capability_list_without_its_flush_is_fatal() {
    let f = Fixture::new("v2");
    let up = f.upload_pack();
    let expect = (String::new(), "fatal: expected flush after capabilities\n".to_owned(), 128);
    for cut in ["1", "3"] {
        assert_eq!(f.run(cut, &["ls-remote", &up, "../up"]), expect);
        assert_eq!(f.run(cut, &["fetch", &up, "../up"]), expect);
        assert_eq!(f.run(cut, &["clone", "-q", &up, "../up", "c"]), expect);
        assert!(!f.work.join("c").exists());
    }
}
