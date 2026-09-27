//! `git tag`'s own config callback, run before anything else the verb does.
//!
//! `cmd_tag()` calls `repo_config(the_repository, git_tag_config, …)`
//! (builtin/tag.c:549) ahead of `parse_options()`, and `git_tag_config()`
//! (builtin/tag.c:210-237) reads `tag.gpgsign` and `tag.forcesignannotated`
//! through `git_config_bool()`, refuses a valueless `tag.sort` with
//! `config_error_nonbool()`, and only then falls through to `git_color_config`
//! and `git_default_config`. zvcs ran the colour/default pair alone, so a bad
//! `tag.gpgSign` listed tags, deleted them, or answered `-h`; a valueless
//! `tag.sort` came out as `fatal: malformed field name:`; and a bad `color.ui`
//! after a bad `tag.gpgSign` was reported first.
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
            .join(format!("zvcs-tag-config-callback-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "base\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
        f.run(&["tag", "v1"]);
        f
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
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
}

fn fatal(line: &str) -> (String, String, i32) {
    (String::new(), format!("fatal: {line}\n"), 128)
}

#[test]
fn the_signing_booleans_stop_every_mode() {
    let f = Fixture::new("booleans");
    for (key, lower) in [
        ("tag.gpgSign", "tag.gpgsign"),
        ("tag.forceSignAnnotated", "tag.forcesignannotated"),
    ] {
        let want = fatal(&format!("bad boolean config value 'bogus' for '{lower}'"));
        let setting = format!("{key}=bogus");
        for mode in [&[][..], &["-l"], &["-d", "v1"], &["-h"]] {
            let mut args = vec!["-c", setting.as_str(), "tag"];
            args.extend_from_slice(mode);
            assert_eq!(f.run(&args), want, "{key} {mode:?}");
        }
    }
    // Nothing was deleted.
    assert_eq!(f.run(&["tag", "-l"]).0, "v1\n");
    // A value git reads as a boolean passes.
    let (out, err, code) = f.run(&["-c", "tag.forceSignAnnotated=yes", "tag", "-l"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("v1\n", "", 0));
}

#[test]
fn a_valueless_sort_is_a_nonbool_refusal() {
    let f = Fixture::new("sort");
    assert_eq!(
        f.run(&["-c", "tag.sort", "tag"]),
        (
            String::new(),
            "error: missing value for 'tag.sort'\n\
             fatal: unable to parse 'tag.sort' from command-line config\n"
                .to_owned(),
            128
        )
    );
}

/// One callback, walked value by value: the first refused value in config
/// order is the one reported, whichever layer of the chain refuses it.
#[test]
fn the_chain_reports_in_config_order() {
    let f = Fixture::new("order");
    assert_eq!(
        f.run(&["-c", "tag.gpgSign=bogus", "-c", "color.ui=bogus", "tag"]),
        fatal("bad boolean config value 'bogus' for 'tag.gpgsign'")
    );
    assert_eq!(
        f.run(&["-c", "color.ui=bogus", "-c", "tag.gpgSign=bogus", "tag"]),
        fatal("bad boolean config value 'bogus' for 'color.ui'")
    );
}

#[test]
fn a_bad_column_mode_is_refused_before_help() {
    let f = Fixture::new("column");
    assert_eq!(
        f.run(&["-c", "column.tag=bogus", "tag", "-h"]),
        (
            String::new(),
            "error: unsupported option 'bogus'\n\
             error: invalid column.tag mode bogus\n\
             fatal: unable to parse 'column.tag' from command-line config\n"
                .to_owned(),
            128
        )
    );
}
