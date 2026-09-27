//! How `core.fsync` is split into component names, seen through its warnings.
//!
//! `parse_fsync_components()` (environment.c:237-290) skips `", \t\n\r"` and
//! then takes a name up to the next comma only, matches it as a prefix of every
//! entry of `fsync_component_names[]` (`strncmp(n->name, string, len)`), treats
//! `none` specially only when it is the whole rest of the value, and warns
//! `invalid value for variable core.fsync` for a lone `-`. zvcs split on
//! whitespace too and matched whole names, so `ind` and `bogus index` warned
//! about the wrong thing, a lone `-` warned about an empty component, and
//! `none,x` was taken for `none`.
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
        let root = std::env::temp_dir().join(format!("zvcs-config-fsync-components-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["commit", "-q", "-m", "i"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@example.com")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// The stderr of `git -c core.fsync=<value> read-tree HEAD`, which writes the index.
    fn read_tree_with(&self, value: &str) -> String {
        let (out, err, code) = self.run(&["-c", &format!("core.fsync={value}"), "read-tree", "HEAD"]);
        assert_eq!((out.as_str(), code), ("", 0), "{value}");
        err
    }
}

fn unknown(name: &str) -> String {
    format!("warning: ignoring unknown core.fsync component '{name}'\n")
}

#[test]
fn names_end_at_commas_and_match_as_prefixes() {
    let f = Fixture::new("names");
    assert_eq!(f.read_tree_with("ind"), "");
    assert_eq!(f.read_tree_with("pack,-loose,comm"), "");
    assert_eq!(f.read_tree_with("bogus index"), unknown("bogus index"));
    assert_eq!(f.read_tree_with(",,bogus,, pack"), unknown("bogus"));
    assert_eq!(f.read_tree_with("objects,none,x"), unknown("none") + &unknown("x"));
    assert_eq!(f.read_tree_with("objects,none"), "");
}

#[test]
fn a_lone_minus_is_an_invalid_value_and_ends_the_list() {
    let f = Fixture::new("minus");
    let invalid = "warning: invalid value for variable core.fsync\n";
    assert_eq!(f.read_tree_with("-"), invalid);
    assert_eq!(f.read_tree_with("-,bogus"), invalid);
}
