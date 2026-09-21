//! `GIT_CONFIG_PARAMETERS` and `--config-env`: which bytes are the key, and what a
//! malformed list does.
//!
//! `parse_config_env_list()` (config.c:679-725) has three forms, and which one an entry is
//! decides whether its key may contain an `=`:
//!
//! ```text
//! 'key=value'          old style — the quoted run is split at its FIRST '='
//! 'key'='value'        new style — the quoted key is taken whole, '=' and all
//! 'key'=               new style, implicit bool — the key is whole, the value NULL
//! ```
//!
//! So `'key.with=equals.oldbool'` sets `key.with` to `equals.oldbool`, while
//! `'key.with=equals.newbool'=` is one key named `key.with=equals.newbool` with no value.
//! `--config-env` splits its spec at the LAST `=` (`git_config_push_env()`,
//! config.c:492), so it can hand over such a key too.
//!
//! Anything else is `error(_("bogus format in %s"))`, which returns -1 all the way to
//! `config_with_options()` and becomes `die(_("unable to parse command-line config"))`
//! (config.c:1601-1602) — exit 128, not a warning the command carries on past.
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
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("zvcs-cfg-param-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root };
        let out = f.cmd(&["init", "-q", "-b", "main", "."]).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        // The alias body runs a bare `git`, which must be this binary rather than
        // whatever stock git the machine has installed.
        let bin_dir = std::path::Path::new(BIN).parent().unwrap();
        let path = match std::env::var_os("PATH") {
            Some(p) => {
                let mut dirs = vec![bin_dir.to_path_buf()];
                dirs.extend(std::env::split_paths(&p));
                std::env::join_paths(dirs).unwrap()
            }
            None => bin_dir.as_os_str().to_owned(),
        };
        c.args(args)
            .current_dir(&self.root)
            .env("PATH", path)
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

    /// `GIT_CONFIG_PARAMETERS=<params> git config --get-regexp <pattern>`.
    fn params(&self, params: &str, pattern: &str) -> (String, String, i32) {
        let out = self
            .cmd(&["config", "--get-regexp", pattern])
            .env("GIT_CONFIG_PARAMETERS", params)
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

/// A new-style entry's key is taken whole, so an `=` inside it stays in the key rather
/// than splitting off into the value.
#[test]
fn a_new_style_key_may_contain_equals() {
    let f = Fixture::new("newstyle");
    let params = "'key.one'='foo'  'key.two'='bar' 'key.ambiguous=section.whatever'='value'";
    let (out, err, code) = f.params(params, "key.*");
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("key.one foo\nkey.two bar\nkey.ambiguous=section.whatever value\n", "", 0)
    );
}

/// The old and new spellings of a bool disagree about the same bytes: without the
/// trailing `=` the run is split at its first `=`, with it the key is whole and valueless.
#[test]
fn old_and_new_bools_split_the_same_bytes_differently() {
    let f = Fixture::new("bools");
    let params = "'key.with=equals.oldbool' 'key.with=equals.newbool'=";
    let (out, err, code) = f.params(params, "key.*");
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("key.with equals.oldbool\nkey.with=equals.newbool\n", "", 0)
    );
}

/// A quoted run may carry an embedded quote as `'\''`, and that is not a malformed list.
#[test]
fn an_embedded_quote_is_not_a_bogus_list() {
    let f = Fixture::new("quote");
    let (out, err, code) = f.params(r"'env.one=one'\''' 'env.two=two'", "env.*");
    assert_eq!((out.as_str(), err.as_str(), code), ("env.one one'\nenv.two two\n", "", 0));
}

/// A list that does not parse is fatal, not a line on stderr the command carries on past.
#[test]
fn a_bogus_list_is_fatal() {
    let f = Fixture::new("bogus");
    let (out, err, code) = f.params(r"'env.one=one'\ 'env.two=two'", "env.*");
    assert_eq!(out, "", "nothing may be printed from a list that did not parse");
    assert!(err.contains("bogus format in GIT_CONFIG_PARAMETERS"), "{err}");
    assert!(err.contains("fatal: unable to parse command-line config"), "{err}");
    assert_eq!(code, 128);
}

/// `--config-env` splits at the last `=`, so the key keeps every earlier one.
#[test]
fn config_env_splits_at_the_last_equals() {
    let f = Fixture::new("env");
    let out = f
        .cmd(&[
            "--config-env=section.subsection=with=equals.key=ENVVAR",
            "config",
            "section.subsection=with=equals.key",
        ])
        .env("ENVVAR", "value=with=equals")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "value=with=equals\n");
    assert_eq!(out.status.code(), Some(0));
}

/// An override a parent `git` handed down is counted once, not once per channel: the
/// child inherits `GIT_CONFIG_PARAMETERS` and must not re-publish it alongside its own.
#[test]
fn an_inherited_override_is_not_counted_twice() {
    let f = Fixture::new("inherit");
    let out = f
        .cmd(&["config", "alias.x", r"!git -c x.two=2 config --get-regexp ^x\.*"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let out = f.cmd(&["-c", "x.one=1", "x"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "x.one 1\nx.two 2\n");
    assert_eq!(out.status.code(), Some(0));
}
