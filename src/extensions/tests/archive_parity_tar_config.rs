//! `init_tar_archiver()`'s configuration walk.
//!
//! `cmd_archive()` calls `init_archivers()` straight after its own
//! `-o`/`--remote`/`--exec` pass (builtin/archive.c:97-100), and
//! `init_tar_archiver()` reads the configuration with
//! `repo_config(the_repository, git_tar_config, NULL)` (archive-tar.c:529-545):
//! once per configured value, in order, whatever the command line goes on to
//! ask for. `tar.umask` is `git_config_int()` unless it is `user`
//! (archive-tar.c:415-429); `tar.<name>.command` refuses a valueless key with
//! `config_error_nonbool()`, which `configset_iter()` turns into
//! `git_die_config_linenr()`; `tar.<name>.remote` is `git_config_bool()`
//! (archive-tar.c:375-413).
//!
//! zvcs read the keys lazily, last value only: a valueless
//! `tar.<name>.command` registered a format, a bad `tar.<name>.remote` was
//! silently false, a bad `tar.umask` superseded by a good one passed, a
//! valueless `tar.umask` took the default, and none of them stopped `--list`,
//! `-h` or `-o`, which git refuses before looking at any of them.
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
    /// One commit holding `file`.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("zvcs-archive-tar-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("file"), "x\n").unwrap();
        f.run(&["add", "file"]);
        f.run(&["commit", "-q", "-m", "base"]);
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

    fn append_config(&self, text: &str) -> usize {
        let path = self.work.join(".git/config");
        let mut config = std::fs::read_to_string(&path).unwrap();
        config.push_str(text);
        std::fs::write(&path, &config).unwrap();
        config.lines().count()
    }
}

const NONBOOL_CMDLINE: &str = "error: missing value for 'tar.x.command'\n\
                               fatal: unable to parse 'tar.x.command' from command-line config\n";

#[test]
fn a_valueless_filter_command_dies_before_anything_else() {
    let f = Fixture::new("command");
    for args in [
        &["archive", "--list"][..],
        &["archive", "-h"][..],
        &["archive", "--bogus"][..],
        &["archive", "HEAD"][..],
    ] {
        let mut all = vec!["-c", "tar.x.command"];
        all.extend_from_slice(args);
        assert_eq!(f.run(&all), (String::new(), NONBOOL_CMDLINE.into(), 128), "{args:?}");
    }
    // `-o` has not been created yet.
    assert_eq!(
        f.run(&["-c", "tar.x.command", "archive", "-o", "out.tar", "HEAD"]),
        (String::new(), NONBOOL_CMDLINE.into(), 128)
    );
    assert!(!f.work.join("out.tar").exists());

    // From a file, the refusal names the line.
    let line = f.append_config("[tar \"x\"]\n\tcommand\n");
    let want = format!(
        "error: missing value for 'tar.x.command'\n\
         fatal: bad config variable 'tar.x.command' in file '.git/config' at line {line}\n"
    );
    assert_eq!(f.run(&["archive", "--list"]), (String::new(), want, 128));
}

#[test]
fn a_non_boolean_filter_remote_dies() {
    let f = Fixture::new("remote");
    assert_eq!(
        f.run(&["-c", "tar.x.remote=bogus", "archive", "--list"]),
        (String::new(), "fatal: bad boolean config value 'bogus' for 'tar.x.remote'\n".into(), 128)
    );
    // A filter that never had a command is not registered, whatever else it has.
    assert_eq!(
        f.run(&["-c", "tar.x.remote", "archive", "--list"]),
        ("tar\ntgz\ntar.gz\nzip\n".into(), String::new(), 0)
    );
}

#[test]
fn every_umask_value_is_parsed() {
    let f = Fixture::new("umask");
    // The first value is refused even though a later one would have been fine.
    assert_eq!(
        f.run(&["-c", "tar.umask=bogus", "-c", "tar.umask=0", "archive", "HEAD"]),
        (
            String::new(),
            "fatal: bad numeric config value 'bogus' for 'tar.umask': invalid unit\n".into(),
            128
        )
    );
    // Valueless is `git_config_int(var, NULL)`: the empty string's invalid unit.
    assert_eq!(
        f.run(&["-c", "tar.umask", "archive", "--list"]),
        (String::new(), "fatal: bad numeric config value '' for 'tar.umask': invalid unit\n".into(), 128)
    );
    f.append_config("[tar]\n\tumask = 1x\n");
    assert_eq!(
        f.run(&["archive", "--list"]),
        (
            String::new(),
            "fatal: bad numeric config value '1x' for 'tar.umask' in file .git/config: invalid unit\n"
                .into(),
            128
        )
    );
}
