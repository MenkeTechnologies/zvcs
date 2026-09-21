//! Two pretty-format defects: `--pretty=reference`'s date and the boolean
//! grammar `%(trailers:<opt>=<v>)` accepts.
//!
//! `reference` is a built-in user format that carries a date mode of its own:
//!
//! ```c
//! { "reference",	CMIT_FMT_USERFORMAT,	1,	0,
//! 	0, DATE_SHORT, "%C(auto)%h (%s, %ad)" },
//! ```
//! (pretty.c:131-132, v2.55.0)
//!
//! ```c
//! if (!rev->date_mode_explicit && commit_format->default_date_mode_type)
//!         rev->date_mode.type = commit_format->default_date_mode_type;
//! ```
//! (`get_commit_format()`, pretty.c:216-217)
//!
//! The guard is `date_mode_explicit`, which only the `--date=` *option* sets
//! (revision.c), so `log.date` does not keep the format from imposing `short`
//! while `--date=` does. Testing "is the mode still the default" instead — which
//! is what the port did — read a configured `log.date` as an override.
//!
//! The trailers atom reads its boolean options through
//!
//! ```c
//! if (!argval) {
//!         *val = 1;
//!         return 1;
//! }
//!
//! strval = xstrndup(argval, arglen);
//! v = git_parse_maybe_bool(strval);
//! free(strval);
//!
//! if (v == -1)
//!         return 0;
//! ```
//! (`match_placeholder_bool_arg()`, pretty.c:1237-1250)
//!
//! so `yes`/`no`, `on`/`off`, an integer and the empty string are all accepted
//! beside `true`/`false`. The port took the two words alone, and a rejected
//! option makes the whole placeholder print as literal text — so
//! `%(trailers:only=yes)` emitted itself.
#![cfg(unix)]

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
    /// One commit whose message ends in two real trailers and one line that is
    /// not a `key: value` at all.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-log-refsum-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "a\n").unwrap();
        f.git(&["add", "file"]);
        let msg = f.root.join("msg");
        std::fs::write(
            &msg,
            "subject\n\nbody\n\nSigned-off-by: A U Thor <author@example.com>\n\
             Acked-by: X <x@example.com>\nnot a trailer\n",
        )
        .unwrap();
        f.git(&["commit", "-q", "-F", msg.to_str().unwrap()]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1112911993 -0700")
            .env("GIT_COMMITTER_DATE", "1112911993 -0700")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// `log.date` does not reach `--pretty=reference`; `--date=` does.
#[test]
fn reference_takes_its_own_short_date_over_log_date() {
    let f = Fixture::new("refdate");
    let short = f.stdout(&["log", "--pretty=tformat:%h (%s, %as)"]);
    assert!(short.contains("2005-04-07"), "{short}");

    assert_eq!(f.stdout(&["log", "--pretty=reference"]), short);
    assert_eq!(f.stdout(&["-c", "log.date=rfc", "log", "--pretty=reference"]), short);
    assert_eq!(f.stdout(&["-c", "log.date=raw", "log", "--pretty=reference"]), short);

    // An explicit `--date=` is `date_mode_explicit`, so it wins.
    let rfc = f.stdout(&["log", "--pretty=reference", "--date=rfc"]);
    assert!(rfc.contains("Apr 2005"), "{rfc}");
    // …including when it merely restates the default, since the flag is what is
    // being tested, not the value.
    let def = f.stdout(&["-c", "log.date=rfc", "log", "--pretty=reference", "--date=default"]);
    assert!(def.contains("Apr 7 "), "{def}");
}

/// `only=` takes the whole boolean grammar, and a rejected option makes the
/// placeholder print itself.
#[test]
fn trailer_bool_options_take_the_whole_boolean_grammar() {
    let f = Fixture::new("trailbool");
    let only = "Signed-off-by: A U Thor <author@example.com>\nAcked-by: X <x@example.com>\n";
    let all = format!("{only}not a trailer\n");

    for spelling in ["yes", "on", "true", "1"] {
        assert_eq!(
            f.stdout(&["log", "-1", &format!("--pretty=format:%(trailers:only={spelling})")]),
            only,
            "only={spelling}"
        );
    }
    for spelling in ["no", "off", "false", "0", ""] {
        assert_eq!(
            f.stdout(&["log", "-1", &format!("--pretty=format:%(trailers:only={spelling})")]),
            all,
            "only={spelling}"
        );
    }
    // A bare option is true (`if (!argval) { *val = 1; ... }`).
    assert_eq!(f.stdout(&["log", "-1", "--pretty=format:%(trailers:only)"]), only);

    // A value the grammar rejects is not a boolean at all, so the atom is not an
    // atom and the text stands as written.
    assert_eq!(
        f.stdout(&["log", "-1", "--pretty=format:%(trailers:only=bogus)"]),
        "%(trailers:only=bogus)"
    );
}

/// `key=` turns `only_trailers` on as a side effect, so a later `only=no` is what
/// lets a non-trailer line through beside the selected key.
#[test]
fn only_no_after_key_restores_the_nontrailer_lines() {
    let f = Fixture::new("keyonly");
    assert_eq!(
        f.stdout(&["log", "-1", "--pretty=format:%(trailers:key=Acked-by)"]),
        "Acked-by: X <x@example.com>\n"
    );
    assert_eq!(
        f.stdout(&["log", "-1", "--pretty=format:%(trailers:key=foo,only=no)"]),
        "not a trailer\n"
    );
}

/// The other three boolean options read the same grammar.
#[test]
fn unfold_keyonly_and_valueonly_read_it_too() {
    let f = Fixture::new("others");
    assert_eq!(
        f.stdout(&["log", "-1", "--pretty=format:%(trailers:keyonly=yes,only=yes)"]),
        "Signed-off-by\nAcked-by\n"
    );
    assert_eq!(
        f.stdout(&["log", "-1", "--pretty=format:%(trailers:valueonly=on,only=1)"]),
        "A U Thor <author@example.com>\nX <x@example.com>\n"
    );
    // `unfold=off` is the default, so it changes nothing but must still parse.
    assert_eq!(
        f.stdout(&["log", "-1", "--pretty=format:%(trailers:unfold=off,only=yes)"]),
        "Signed-off-by: A U Thor <author@example.com>\nAcked-by: X <x@example.com>\n"
    );
}
