//! `git help` under a `.git/config` the parser refuses.
//!
//! git.c gives `help` no setup flag, so `run_builtin()` reads no configuration;
//! `cmd_help()` reads it only for a topic (builtin/help.c:743-744), for verbose
//! `--all` with aliases shown (`list_aliases()` via help.c:486-492) and for
//! `--aliases-for-completion` (builtin/help.c:721). Every usage refusal of the
//! parse and every other listing mode answers as if the file were sound. zvcs
//! read the repository's config for any non-bare `help` but `-g`, so
//! `help --developer-interfaces x` died at 128 on the bad line where stock
//! refuses the operand at 129.
//!
//! Expectations measured from stock git 2.56.0 under the same fixture.

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A repository whose `.git/config` ends in a stray `]` line.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-help-badcfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        std::fs::create_dir_all(root.join(".git/refs")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            root.join(".git/config"),
            "[core]\n\trepositoryformatversion = 0\n\tbare = false\n]\n",
        )
        .unwrap();
        Fixture { root: root.canonicalize().unwrap() }
    }

    fn help(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg("help")
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", self.root.parent().unwrap())
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .output()
            .unwrap()
    }
}

const BAD_LINE: &str = "fatal: bad config line 4 in file .git/config\n";

/// `opt_mode_usage()` refuses the operand before any read: 129 and the usage
/// block, not the config line at 128.
#[test]
fn mode_operand_refusal_precedes_config() {
    let f = Fixture::new("operand");
    let out = f.help(&["--no-web", "--developer-interfaces", "does-not-exist"]);
    assert_eq!(out.status.code(), Some(129));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.starts_with(
            "fatal: the '--developer-interfaces' option doesn't take any non-option arguments\n\nusage: git help "
        ),
        "{err}"
    );
}

/// An unknown switch is parse-options' refusal, also ahead of any read.
#[test]
fn unknown_switch_precedes_config() {
    let f = Fixture::new("unknown");
    let out = f.help(&["-u"]);
    assert_eq!(out.status.code(), Some(129));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("error: unknown switch `u'\n"));
}

/// The static listings and `--all` without aliases never read configuration.
#[test]
fn static_listings_ignore_the_bad_line() {
    let f = Fixture::new("static");
    for args in [
        &["--developer-interfaces"][..],
        &["--user-interfaces"],
        &["-c"],
        &["--config-for-completion"],
        &["-a", "--no-aliases"],
        &["-a", "--no-verbose"],
    ] {
        let out = f.help(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(!out.stdout.is_empty(), "{args:?}");
        assert!(out.stderr.is_empty(), "{args:?}");
    }
}

/// `list_aliases()` and the topic's `repo_config()` do read it, and die there.
#[test]
fn alias_listings_and_topics_die_on_the_bad_line() {
    let f = Fixture::new("reads");
    for args in [&["-a"][..], &["--aliases-for-completion"], &["status"]] {
        let out = f.help(args);
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), BAD_LINE, "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}
