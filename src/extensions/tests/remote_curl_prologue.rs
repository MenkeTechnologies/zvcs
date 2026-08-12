//! `remote-curl`'s argument prologue, which four subcommand names share.
//!
//! Stock git installs one executable — `remote-curl.c` — under four names, and
//! says so in its own source (`remote-curl.c:1579-1583`, "folding all the
//! various aliases (`git-remote-http`, `git-remote-https`, and etc.) here since
//! they are all just copies of the same actual executable"). Everything before
//! the first line of the helper protocol is therefore identical for all four,
//! and this file pins that: the same argv produces the same bytes and the same
//! exit code under `remote-http`, `remote-https`, `remote-ftp` and
//! `remote-ftps`. `http-push` reaches the same `credential_from_url()` check
//! from `http-push.c` and is pinned with them.
//!
//! Every expectation is what stock git 2.55.0 printed for the same argv, and is
//! hardcoded rather than compared at run time so the suite needs no `git` on the
//! machine running it. The whole surface is argv and configuration — no socket
//! is opened by any case here, so it is safe on a headless machine with no
//! network.
//!
//! The two cases that look like flags are the interesting ones. `remote-curl`
//! has no options at all: `-h` and `--no-such-flag` are `argv[1]`, i.e. the
//! *remote name*, so they become the URL, gain `end_url_with_slash()`'s trailing
//! slash, and are rejected by `credential_from_url()` for having no scheme —
//! `warning:` then `fatal:`, exit 128, not a usage error. An implementation that
//! treats them as flags gets exit 1 and the usage line, which is what this
//! catches.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// git's usage `error()` for the helper, byte-for-byte.
const USAGE: &str = "error: remote-curl: usage: git remote-curl <remote> [<url>]\n";

/// The four names git installs `remote-curl` under.
const HELPERS: [&str; 4] = ["remote-http", "remote-https", "remote-ftp", "remote-ftps"];

/// An empty repository to run the helpers in. `remote_get()` reads
/// configuration, so the cases that exercise it need a real one.
struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-remotecurl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// Run with stdin closed, so a helper that reaches its command loop hits EOF
    /// immediately instead of waiting for input. Returns `(exit code, stderr)`;
    /// every case here leaves stdout empty, which is asserted for all of them.
    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = self
            .cmd(args)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.stdout.is_empty(),
            "`git {args:?}` wrote to stdout: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        (
            out.status.code().expect("the helper exited rather than being signalled"),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// `if (argc < 2) error(...)`: no remote name at all is the one usage error the
/// helper has, and it exits 1 — not 128, and not 129.
#[test]
fn no_arguments_is_the_usage_error() {
    let f = Fixture::new("noargs");
    for helper in HELPERS {
        assert_eq!(f.run(&[helper]), (1, USAGE.to_string()), "{helper}");
    }
}

/// `-h` is not help: it is `argv[1]`, so it becomes the URL and is rejected for
/// having no scheme. Exit 128, from `credential_from_url()`'s `die()`.
#[test]
fn dash_h_is_a_remote_name_not_a_flag() {
    let f = Fixture::new("dashh");
    let expected = "warning: url has no scheme: -h/\n\
                    fatal: credential url cannot be parsed: -h/\n";
    for helper in HELPERS {
        assert_eq!(f.run(&[helper, "-h"]), (128, expected.to_string()), "{helper}");
    }
    // `http-push` does have a `-h` (it prints its own usage), so the equivalent
    // there is any flag its table does not know: an unrecognised `-…` falls
    // through the option scan and becomes the URL, reaching the same check.
    assert_eq!(
        f.run(&["http-push", "--no-such-flag"]),
        (
            128,
            "warning: url has no scheme: --no-such-flag/\n\
             fatal: credential url cannot be parsed: --no-such-flag/\n"
                .to_string()
        )
    );
}

/// An unknown long option is likewise a remote name, for all four spellings.
#[test]
fn unknown_flag_is_a_remote_name() {
    let f = Fixture::new("unknown");
    let expected = "warning: url has no scheme: --no-such-flag/\n\
                    fatal: credential url cannot be parsed: --no-such-flag/\n";
    for helper in HELPERS {
        assert_eq!(
            f.run(&[helper, "--no-such-flag"]),
            (128, expected.to_string()),
            "{helper}"
        );
    }
}

/// `argv[2]` wins over the remote name, and `argv[3]` and beyond are ignored
/// rather than being a usage error — the C reads `argv[2]` and never looks
/// further.
#[test]
fn surplus_arguments_are_ignored_and_argv2_is_the_url() {
    let f = Fixture::new("surplus");
    let expected = "warning: url has no scheme: second/\n\
                    fatal: credential url cannot be parsed: second/\n";
    for helper in HELPERS {
        assert_eq!(
            f.run(&[helper, "first", "second", "third"]),
            (128, expected.to_string()),
            "{helper}"
        );
    }
}

/// With only `<remote>`, the URL comes from `remote_get()`: the configured
/// `remote.<name>.url`, not the name itself.
#[test]
fn single_argument_resolves_the_configured_remote_url() {
    let f = Fixture::new("remoteurl");
    f.git(&["config", "remote.origin.url", "no-scheme-here"]);
    let expected = "warning: url has no scheme: no-scheme-here/\n\
                    fatal: credential url cannot be parsed: no-scheme-here/\n";
    for helper in HELPERS {
        assert_eq!(f.run(&[helper, "origin"]), (128, expected.to_string()), "{helper}");
    }
}

/// `alias_url()`: the longest `url.<base>.insteadOf` that prefixes the resolved
/// URL is replaced by its `<base>`, and the rewrite happens before the
/// credential parse — so the diagnostic quotes the rewritten URL.
#[test]
fn instead_of_rewrites_the_url_before_the_credential_parse() {
    let f = Fixture::new("insteadof");
    f.git(&["config", "remote.origin.url", "short:repo.git"]);
    f.git(&["config", "url.rewritten-no-scheme/.insteadOf", "short:"]);
    let expected = "warning: url has no scheme: rewritten-no-scheme/repo.git/\n\
                    fatal: credential url cannot be parsed: rewritten-no-scheme/repo.git/\n";
    for helper in HELPERS {
        assert_eq!(f.run(&[helper, "origin"]), (128, expected.to_string()), "{helper}");
    }
}

/// A URL that already ends in `/` does not get a second one — `strbuf_complete`,
/// not an unconditional append. Checked through the diagnostic, which quotes the
/// URL after the slash step.
#[test]
fn trailing_slash_is_not_doubled() {
    let f = Fixture::new("slash");
    let expected = "warning: url has no scheme: no-scheme/\n\
                    fatal: credential url cannot be parsed: no-scheme/\n";
    for helper in HELPERS {
        assert_eq!(
            f.run(&[helper, "ignored", "no-scheme/"]),
            (128, expected.to_string()),
            "{helper}"
        );
    }
}

/// `credential_from_url_1()` checks components in a fixed order and names the
/// first one carrying a newline. The scheme is present here, so this reaches the
/// component scan rather than the no-scheme exit.
#[test]
fn a_newline_in_the_host_is_named_by_component() {
    let f = Fixture::new("newline");
    let expected = "warning: url contains a newline in its host component: https://ho\nst/\n\
                    fatal: credential url cannot be parsed: https://ho\nst/\n";
    for helper in HELPERS {
        assert_eq!(
            f.run(&[helper, "ignored", "https://ho\nst"]),
            (128, expected.to_string()),
            "{helper}"
        );
    }
}
