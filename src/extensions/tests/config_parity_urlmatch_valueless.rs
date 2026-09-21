//! `--get-urlmatch` over a section that also carries a valueless key, and where the
//! winning value's scope comes from.
//!
//! `urlmatch_collect_fn()` (builtin/config.c:828-856) keeps a `value_is_null` flag beside
//! the value and hands `format_config()` NULL for it, so `[http] sslVerify` — set with no
//! `=` — is a candidate like any other: `--bool` reads it as true, and a whole-section
//! listing prints its key with nothing after it. It also stores the entry's own
//! `key_value_info`, so `--show-scope` names the file the winning value came from rather
//! than the command line.
//!
//! The winner is chosen by `cmp_matches()` through `urlmatch_config_entry()`
//! (urlmatch.c:572-618): a section with no subsection carries no URL and matches every
//! one at the lowest specificity there is, so an exact `[http "https://weak.example.com"]`
//! outranks it for that host and nothing else does.
//!
//! Every expectation was measured from stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

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
    fn new(tag: &str, config: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-cfg-um-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        let out = f.cmd(&["init", "-q", "-b", "main", "."]).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        std::fs::write(f.root.join(".git/config"), config).unwrap();
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env_remove("GIT_CONFIG")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_CONFIG_COUNT")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    /// `git config <args…>`.
    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let mut full = vec!["config"];
        full.extend_from_slice(args);
        let out = self.cmd(&full).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

const EXACT: &str = "[http]\n\
                     \tsslVerify\n\
                     [http \"https://weak.example.com\"]\n\
                     \tsslVerify = false\n\
                     \tcookieFile = /tmp/cookie.txt\n";

/// A valueless key in the bare `[http]` section is a candidate, so a URL nothing else
/// matches falls back to it and `--bool` reads it as true.
#[test]
fn a_valueless_key_is_a_candidate() {
    let f = Fixture::new("valueless", EXACT);

    for args in [
        ["--bool", "--get-urlmatch", "http.SSLverify", "https://good.example.com"].as_slice(),
        ["get", "--bool", "--url=https://good.example.com", "http.SSLverify"].as_slice(),
    ] {
        let (out, err, code) = f.run(args);
        assert_eq!((out.as_str(), err.as_str(), code), ("true\n", "", 0), "{args:?}");
    }

    // The exact subsection outranks it for the host it names.
    let (out, _, code) =
        f.run(&["--bool", "--get-urlmatch", "http.sslverify", "https://weak.example.com"]);
    assert_eq!((out.as_str(), code), ("false\n", 0));
}

/// A key nothing in the section is named `doesnt.exist` under is exit 1 with no output —
/// not the bare section's fallback.
#[test]
fn an_unnamed_key_is_exit_one() {
    let f = Fixture::new("missing", EXACT);
    let (out, _, code) =
        f.run(&["--bool", "--get-urlmatch", "doesnt.exist", "https://good.example.com"]);
    assert_eq!((out.as_str(), code), ("", 1));
}

/// A whole-section query prints `section.key value` for each winner, sorted by key, and a
/// valueless winner prints its key alone.
#[test]
fn a_whole_section_query_lists_every_winner() {
    let f = Fixture::new("section", EXACT);

    let (out, _, code) = f.run(&["--get-urlmatch", "HTTP", "https://weak.example.com"]);
    assert_eq!(
        (out.as_str(), code),
        ("http.cookiefile /tmp/cookie.txt\nhttp.sslverify false\n", 0)
    );

    // Only the bare section matches this host, and its one key has no value.
    let (out, _, code) = f.run(&["--get-urlmatch", "HTTP", "https://other.example.org"]);
    assert_eq!((out.as_str(), code), ("http.sslverify\n", 0));
}

/// `--show-scope` names the file the winning value came from.
#[test]
fn show_scope_names_the_winners_own_file() {
    let f = Fixture::new(
        "scope",
        "[http \"https://weak.example.com\"]\n\tsslVerify = false\n\tcookieFile = /tmp/cookie.txt\n",
    );

    for args in [
        ["--get-urlmatch", "--show-scope", "HTTP", "https://weak.example.com"].as_slice(),
        ["get", "--url=https://weak.example.com", "--show-scope", "HTTP"].as_slice(),
    ] {
        let (out, err, code) = f.run(args);
        assert_eq!(
            (out.as_str(), err.as_str(), code),
            ("local\thttp.cookiefile /tmp/cookie.txt\nlocal\thttp.sslverify false\n", "", 0),
            "{args:?}"
        );
    }
}

/// A `*` in the subsection matches one host label, and a more specific exact match still
/// beats it.
#[test]
fn a_wildcard_subsection_matches_one_label() {
    let f = Fixture::new(
        "wildcard",
        "[http]\n\
         \tsslVerify\n\
         [http \"https://*.example.com\"]\n\
         \tsslVerify = false\n\
         \tcookieFile = /tmp/cookie.txt\n",
    );

    // `*` stands for exactly one label: no label at all, a label that merely contains
    // `example`, a second label in front of it, and a longer suffix all miss, and fall
    // back to the bare section's valueless key.
    for url in [
        "https://example.com",
        "https://good-example.com",
        "https://deep.nested.example.com",
        "https://more.example.com.au",
    ] {
        let (out, _, code) = f.run(&["--bool", "--get-urlmatch", "http.sslverify", url]);
        assert_eq!((out.as_str(), code), ("true\n", 0), "{url}");
    }

    let (out, _, code) =
        f.run(&["--bool", "--get-urlmatch", "http.sslverify", "https://good.example.com"]);
    assert_eq!((out.as_str(), code), ("false\n", 0));

    // A URL the wildcard misses sees only the bare section, whose one key is valueless.
    let (out, _, code) = f.run(&["--get-urlmatch", "HTTP", "https://more.example.com.au"]);
    assert_eq!((out.as_str(), code), ("http.sslverify\n", 0));
}
