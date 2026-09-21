//! Five reading edges of `git config`, each measured from stock git 2.55.0.
//!
//!   * An old-fashioned `[section.SubSection]` header is read by `get_base_var()`
//!     (config.c:983-997), which lower-cases every byte of it — the dot included — so its
//!     subsection is case-insensitive, unlike the quoted `[section "SubSection"]` form
//!     that `get_extended_base_var()` (config.c:943-981) copies verbatim. A file may
//!     carry both, naming two different keys.
//!   * `--type=path`, `--type=color` and `--expiry-date` all open with
//!     `if (!value) return config_error_nonbool(var);` (config.c:1316, 1361, 1352), so a
//!     key written with no `=` is `missing value for '<key>'` followed by the parse's own
//!     `bad config line <n> in file <path>` — exit 128, not an empty line.
//!   * `--show-origin` puts the file name through `quote_c_style()`
//!     (`show_config_origin()`, builtin/config.c:243), so a name holding a double quote
//!     is quoted and escaped; `--null` writes it raw instead.
//!   * `--get-color` never parses its slot (`git_get_color_config()`,
//!     builtin/config.c:759): an empty one simply matches nothing and falls through to
//!     the default, and `git config get --type=color --default=<c> ""` is the subcommand
//!     spelling of the same thing (builtin/config.c:1094-1096).
//!   * `--replace-all` is `check_argc(argc, 2, 3)` (builtin/config.c:1541), so it refuses
//!     a lone key rather than setting it to the empty string.
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
            std::env::temp_dir().join(format!("zvcs-cfg-read-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// `git config <args…>`, run in the fixture directory with no repository around.
    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let mut full = vec!["config"];
        full.extend_from_slice(args);
        let mut c = Command::new(BIN);
        c.args(&full)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env_remove("GIT_CONFIG")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_CONFIG_COUNT")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("LC_ALL", "C")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        let out = c.output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

/// The dotted header's subsection folds case; the quoted one's does not, so the two
/// headers below hold two different keys.
#[test]
fn a_dotted_subsection_folds_case_and_a_quoted_one_does_not() {
    let f = Fixture::new("dotted");
    f.write(
        "conf",
        "[section.SubSection]\nkey = one\n[section \"SubSection\"]\nkey = two\n",
    );

    let (out, err, code) = f.run(&["--file", "conf", "--list"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("section.subsection.key=one\nsection.SubSection.key=two\n", "", 0)
    );

    let (out, _, code) = f.run(&["--file", "conf", "section.subsection.key"]);
    assert_eq!((out.as_str(), code), ("one\n", 0));
    let (out, _, code) = f.run(&["--file", "conf", "section.SubSection.key"]);
    assert_eq!((out.as_str(), code), ("two\n", 0));
}

/// The three types with no reading of a valueless key refuse it, naming the key and then
/// the line the parse gave up on.
#[test]
fn types_that_need_a_value_refuse_a_valueless_key() {
    let f = Fixture::new("nonbool");
    f.write("conf", "[bool]\n\tvar\n");

    for ty in ["--path", "--type=path", "--type=color", "--expiry-date"] {
        let (out, err, code) = f.run(&["--file", "conf", ty, "bool.var"]);
        assert_eq!((out.as_str(), code), ("", 128), "{ty}");
        assert!(err.contains("missing value for 'bool.var'"), "{ty}: {err}");
        assert!(err.contains("bad config line 2 in file conf"), "{ty}: {err}");
    }

    // A boolean type still reads it as true — that is the whole point of the spelling.
    let (out, _, code) = f.run(&["--file", "conf", "--bool", "bool.var"]);
    assert_eq!((out.as_str(), code), ("true\n", 0));
}

/// `--show-origin` C-quotes a file name that needs it, and writes it raw under `--null`.
#[test]
fn show_origin_c_quotes_the_file_name() {
    let f = Fixture::new("quote");
    let weird = "file\" (dq) and spaces.conf";
    f.write(weird, "[user]\n\tcustom = true\n");

    let (out, err, code) = f.run(&["--list", "--file", weird, "--show-origin"]);
    assert_eq!(
        (out.as_str(), err.as_str(), code),
        ("file:\"file\\\" (dq) and spaces.conf\"\tuser.custom=true\n", "", 0)
    );

    let (out, _, code) = f.run(&["--list", "--file", weird, "--show-origin", "--null"]);
    assert_eq!((out.as_str(), code), (format!("file:{weird}\0user.custom\ntrue\0").as_str(), 0));
}

/// An empty `--get-color` slot matches nothing and falls through to the default, in both
/// the legacy and the subcommand spelling.
#[test]
fn an_empty_get_color_slot_uses_the_default() {
    let f = Fixture::new("color");
    f.write("conf", "[user]\n\tname = x\n");

    let (out, err, code) = f.run(&["--file", "conf", "--get-color", "", "red"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("\u{1b}[31m", "", 0));

    let (out, err, code) =
        f.run(&["get", "--file", "conf", "--type=color", "--default=red", ""]);
    assert_eq!((out.as_str(), err.as_str(), code), ("\u{1b}[31m", "", 0));
}

/// `--replace-all` with only a key is a usage error, and the file is untouched.
#[test]
fn replace_all_needs_a_value() {
    let f = Fixture::new("argc");
    let path = f.write("conf", "[beta]\n\thaha = x\n");

    let (out, err, code) = f.run(&["--file", "conf", "--replace-all", "beta.haha"]);
    assert_eq!((out.as_str(), code), ("", 129));
    assert!(err.contains("wrong number of arguments, should be from 2 to 3"), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[beta]\n\thaha = x\n");
}
