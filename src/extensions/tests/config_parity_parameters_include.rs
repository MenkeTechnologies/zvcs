//! `git_config_from_parameters()` (config.c:731-790) over the inherited
//! `GIT_CONFIG_COUNT` triple, and the order it reads its two channels in.
//!
//! * An inherited `include.path` with a relative value reaches
//!   `handle_path_include()` (config.c:162-168) with a command-line origin:
//!   `error: relative config includes must come from files`, then
//!   `die(_("unable to parse command-line config"))` (config.c:1601-1602), 128.
//!   zvcs let gitoxide load it and exited 1 with its own message.
//! * `includeIf.gitdir:./<x>.<key>` from the command line is
//!   `prepare_include_condition_pattern()`'s `error()` (config.c:214-219),
//!   printed whatever the key; its `-1` is truthy in `git_config_include()`'s
//!   `&&` chain (config.c:436-438), so a relative `path` is refused a second
//!   time and an absolute one is followed with the command carrying on.
//! * The `GIT_CONFIG_COUNT` entries are read before `GIT_CONFIG_PARAMETERS`,
//!   which is where every `-c` lives, so the inherited entry's error wins.
//! * For a builtin that read is repository setup, after the
//!   `$GIT_OBJECT_DIRECTORY` discovery refusal; and `stripspace` never reads
//!   configuration, so a malformed `-c` does not stop it.
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
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-config-parameters-include-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "."], &[]);
        f
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_OBJECT_DIRECTORY");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

const DIE: &str = "fatal: unable to parse command-line config\n";
const RELATIVE: &str = "error: relative config includes must come from files\n";
const CONDITIONAL: &str = "error: relative config include conditionals must come from files\n";

fn count(key: &'static str, value: &'static str) -> [(&'static str, &'static str); 3] {
    [("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", key), ("GIT_CONFIG_VALUE_0", value)]
}

#[test]
fn an_inherited_relative_include_is_refused_like_a_dash_c_one() {
    let f = Fixture::new("count");
    for args in [&["config", "--get", "core.bare"][..], &["status"][..]] {
        let got = f.run(args, &count("include.path", "foo"));
        assert_eq!(got, (String::new(), format!("{RELATIVE}{DIE}"), 128), "{args:?}");
    }
}

#[test]
fn a_relative_gitdir_condition_is_an_error_that_still_holds() {
    let f = Fixture::new("cond");
    // A relative path: the condition's error, then the include's, then the die.
    let both = (String::new(), format!("{CONDITIONAL}{RELATIVE}{DIE}"), 128);
    assert_eq!(f.run(&["-c", "includeIf.gitdir:./.path=foo", "config", "--get", "core.bare"], &[]), both);
    assert_eq!(f.run(&["config", "--get", "core.bare"], &count("includeIf.gitdir:./.path", "foo")), both);

    // An absolute path that does not exist: one error line and the command runs.
    let one = ("false\n".to_owned(), CONDITIONAL.to_owned(), 0);
    let missing = f.root.join("missing").to_string_lossy().into_owned();
    let key = format!("includeIf.gitdir:./.path={missing}");
    assert_eq!(f.run(&["-c", &key, "config", "--get", "core.bare"], &[]), one);
    // The condition is judged before the key is, so a key other than `path` has it too.
    assert_eq!(f.run(&["-c", "includeIf.gitdir:./.foo=bar", "config", "--get", "core.bare"], &[]), one);

    // An absolute path that exists is followed.
    let inc = f.root.join("inc");
    std::fs::write(&inc, "[user]\n\tname = Included\n").unwrap();
    let key = format!("includeIf.gitdir:./.path={}", inc.display());
    assert_eq!(
        f.run(&["-c", &key, "config", "--get", "user.name"], &[]),
        ("Included\n".to_owned(), CONDITIONAL.to_owned(), 0)
    );
}

#[test]
fn the_inherited_entries_are_read_before_dash_c_and_after_discovery() {
    let f = Fixture::new("order");
    assert_eq!(
        f.run(&["-c", "bad=1", "config", "--get", "core.bare"], &count("include.path", "foo")),
        (String::new(), format!("{RELATIVE}{DIE}"), 128)
    );
    let missing = f.root.join("no-objects").to_string_lossy().into_owned();
    assert_eq!(
        f.run(&["-c", "bad=1", "status"], &[("GIT_OBJECT_DIRECTORY", &missing)]),
        (
            String::new(),
            "fatal: not a git repository (or any of the parent directories): .git\n".to_owned(),
            128
        )
    );
    assert_eq!(f.run(&["-c", "bad=1", "stripspace"], &[]), (String::new(), String::new(), 0));
}
