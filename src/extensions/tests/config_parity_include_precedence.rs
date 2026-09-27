//! Which value wins when a `-c include.path` file and another `-c` set one key.
//!
//! `git_config_from_parameters()` (config.c:731-790) reads the overrides in the
//! order given and `git_config_include()` (:416-448) reads an included file
//! where its line stands, so the last one read wins:
//! `-c core.abbrev=9 -c include.path=<file setting 12>` is 12, and a `-c` after
//! the include beats the file again.
//!
//! gitoxide resolved includes only in its environment layer and appended the
//! `Source::Cli` layer after it without resolving any, and folded every `-c`
//! into one section per name, so every `-c` beat every included value
//! whatever the order.
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
    /// One commit on `master`, and `ab.cfg` setting `core.abbrev = 12` outside the work tree.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-config-include-precedence-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let work = root.join("work");
        std::fs::write(root.join("ab.cfg"), "[core]\n\tabbrev = 12\n").unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "master", "."]);
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

    /// The abbreviated length `rev-parse --short` picks under `overrides`.
    fn short_len(&self, overrides: &[&str]) -> usize {
        let mut args: Vec<&str> = Vec::new();
        for o in overrides {
            args.extend(["-c", o]);
        }
        args.extend(["rev-parse", "--short", "HEAD"]);
        let (out, err, code) = self.run(&args);
        assert_eq!((err.as_str(), code), ("", 0), "{overrides:?}");
        out.trim_end().len()
    }
}

#[test]
fn the_value_read_last_wins() {
    let f = Fixture::new("order");
    let inc = format!("include.path={}", f.root.join("ab.cfg").display());
    let on = format!("includeIf.onbranch:master.path={}", f.root.join("ab.cfg").display());
    let off = format!("includeIf.onbranch:nope.path={}", f.root.join("ab.cfg").display());
    assert_eq!(f.short_len(&["core.abbrev=9", &inc]), 12);
    assert_eq!(f.short_len(&[&inc, "core.abbrev=9"]), 9);
    assert_eq!(f.short_len(&["core.abbrev=9", &inc, "core.abbrev=10"]), 10);
    assert_eq!(f.short_len(&["core.abbrev=9", &on]), 12);
    assert_eq!(f.short_len(&["core.abbrev=9", &off]), 9);
    assert_eq!(f.run(&["-c", "core.abbrev=9", "-c", &inc, "config", "core.abbrev"]), ("12\n".into(), String::new(), 0));
}
