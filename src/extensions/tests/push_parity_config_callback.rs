//! `git_push_config()` (builtin/push.c:477-540) is a config callback, run by
//! `repo_config(the_repository, git_push_config, &flags)` at builtin/push.c:721,
//! before `parse_options()` and before any remote is looked up. A value it
//! refuses stops the push with nothing sent:
//!
//! * `push.followtags`, `push.autosetupremote`, `push.useforceifincludes` and
//!   `submodule.recurse` go through `git_config_bool()`, which dies with
//!   `bad boolean config value`.
//! * `push.gpgsign` returns `error(_("invalid value for '%s'"), k)` for a value
//!   that is neither a boolean nor `if-asked`, so `configset_iter()` follows it
//!   with `git_die_config_linenr()` naming the source.
//! * `push.recursesubmodules` goes through `parse_push_recurse()`
//!   (submodule-config.c:498-526) with `die_on_error`: any true value — the
//!   valueless spelling included, printed as `(null)` — is refused.
//! * `push.pushoption` is `parse_transport_option()` (transport.c:1144-1154):
//!   valueless is `config_error_nonbool`.
//! * `color.push.<slot>` for `reset`/`error` is `color_parse()`.
//!
//! zvcs read these keys through lookups that ignored anything unparsable and
//! pushed anyway (or, for `push.gpgSign`, died with a different message).
//!
//! Expectations measured from stock git 2.55.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

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
    /// A bare `up.git` and a work repository `w` with one commit on `main` and a
    /// remote `o` pointing at `../up.git`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-push-config-cb-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("w");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run_in(&f.root, &["init", "-q", "--bare", "up.git"]);
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "a"]);
        f.run(&["remote", "add", "o", "../up.git"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        self.run_in(&self.work, args)
    }

    fn run_in(&self, dir: &std::path::Path, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.root)
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

    /// The refs the bare remote holds — empty when nothing was pushed.
    fn remote_refs(&self) -> String {
        self.run(&["--git-dir=../up.git", "for-each-ref"]).0
    }

    fn push_with(&self, config: &str) -> (String, String, i32) {
        self.run(&["-c", config, "push", "o", "main"])
    }
}

#[test]
fn boolean_keys_die_before_anything_is_pushed() {
    let f = Fixture::new("bools");
    for (key, lower) in [
        ("push.followTags", "push.followtags"),
        ("push.autoSetupRemote", "push.autosetupremote"),
        ("push.useForceIfIncludes", "push.useforceifincludes"),
        ("submodule.recurse", "submodule.recurse"),
    ] {
        let (out, err, code) = f.push_with(&format!("{key}=bogus"));
        let want = format!("fatal: bad boolean config value 'bogus' for '{lower}'\n");
        assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128), "{key}");
    }
    assert_eq!(f.remote_refs(), "");
}

#[test]
fn gpg_sign_names_the_source_of_the_bad_value() {
    let f = Fixture::new("gpgsign");
    let (out, err, code) = f.push_with("push.gpgSign=bogus");
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "error: invalid value for 'push.gpgsign'\n\
             fatal: unable to parse 'push.gpgsign' from command-line config\n",
            128
        )
    );

    let config = f.work.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("[push]\n\tgpgSign = maybe\n");
    std::fs::write(&config, &text).unwrap();
    let line = text.lines().count();
    let (out, err, code) = f.run(&["push", "o", "main"]);
    let want = format!(
        "error: invalid value for 'push.gpgsign'\n\
         fatal: bad config variable 'push.gpgsign' in file '.git/config' at line {line}\n"
    );
    assert_eq!((out.as_str(), err.as_str(), code), ("", want.as_str(), 128));
    assert_eq!(f.remote_refs(), "");
}

#[test]
fn recurse_submodules_refuses_every_true_value() {
    let f = Fixture::new("recurse");
    // The valueless spelling is `git_parse_maybe_bool(NULL) == 1`.
    let (out, err, code) = f.run(&["-c", "push.recurseSubmodules", "push", "o", "main"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("", "fatal: bad push.recursesubmodules argument: (null)\n", 128)
    );
    let (_, err, code) = f.push_with("push.recurseSubmodules=yes");
    assert_eq!(
        (err.as_str(), code),
        ("fatal: bad push.recursesubmodules argument: yes\n", 128)
    );
    assert_eq!(f.remote_refs(), "");
    // `check` is one of the three named modes and pushes normally.
    let (_, _, code) = f.push_with("push.recurseSubmodules=check");
    assert_eq!(code, 0);
    assert!(f.remote_refs().ends_with("commit\trefs/heads/main\n"));
}

#[test]
fn push_option_and_color_slots_refuse_through_the_config_source() {
    let f = Fixture::new("nonbool");
    let (out, err, code) = f.run(&["-c", "push.pushOption", "push", "o", "main"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        (
            "",
            "error: missing value for 'push.pushoption'\n\
             fatal: unable to parse 'push.pushoption' from command-line config\n",
            128
        )
    );
    let (_, err, code) = f.push_with("color.push.error=bogus");
    assert_eq!(
        (err.as_str(), code),
        (
            "error: invalid color value: bogus\n\
             fatal: unable to parse 'color.push.error' from command-line config\n",
            128
        )
    );
    let (_, err, code) = f.push_with("color.push=bogus");
    assert_eq!(
        (err.as_str(), code),
        ("fatal: bad boolean config value 'bogus' for 'color.push'\n", 128)
    );
    assert_eq!(f.remote_refs(), "");
    // An unknown slot is skipped (`parse_push_color_slot()` < 0 returns 0).
    let (_, _, code) = f.push_with("color.push.other=bogus");
    assert_eq!(code, 0);
}
