//! Where a command-line or environment `include.path` puts the included file.
//!
//! `git_config_include()` (config.c:416-448) passes each value on and then, for
//! an include line, reads the included file right there through
//! `handle_path_include()` (:142-191). `git_config_from_parameters()`
//! (:731-790) hands the `GIT_CONFIG_COUNT` triple and then every `-c` over one
//! entry at a time, so `-c x.y=a -c include.path=<f> -c x.y=b` walks `a`, the
//! include line, the file's values, then `b`.
//!
//! zvcs walked the file's values ahead of every `-c` (they came from the
//! environment copy of the override, which the command-line replay dropped
//! without its include), and gitoxide's environment layer folded every key into
//! the first section of its name, so an inherited triple walked
//! `x.y=1 x.y=3 z.w=2` and put an include behind every later entry of its
//! section.
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
    /// A repository, `inc.cfg` (which includes `inc2.cfg` between its two
    /// values) and `ab.cfg`, all outside the work tree.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-config-include-position-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let work = root.join("work");
        std::fs::write(root.join("inc.cfg"), "[x]\n\ty = inc\n[include]\n\tpath = inc2.cfg\n[x]\n\ty = after\n").unwrap();
        std::fs::write(root.join("inc2.cfg"), "[x]\n\ty = nested\n").unwrap();
        std::fs::write(root.join("ab.cfg"), "[core]\n\tabbrev = 12\n").unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "-b", "main", "."], &[]);
        f
    }

    fn inc(&self) -> String {
        format!("include.path={}", self.root.join("inc.cfg").display())
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .envs(env.iter().copied())
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
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

const LOCAL: &str = "local\tcore.repositoryformatversion=0\nlocal\tcore.filemode=true\nlocal\tcore.bare=false\n\
local\tcore.logallrefupdates=true\n";

#[test]
fn a_command_line_include_is_walked_where_it_was_given() {
    let f = Fixture::new("cli");
    let inc = f.inc();
    assert_eq!(
        f.run(&["-c", "x.y=a", "-c", &inc, "-c", "x.y=b", "-c", &inc, "config", "--get-all", "x.y"], &[]),
        ("a\ninc\nnested\nafter\nb\ninc\nnested\nafter\n".into(), String::new(), 0)
    );
    assert_eq!(f.run(&["-c", "x.y=a", "-c", &inc, "config", "--get", "x.y"], &[]).0, "after\n");
    assert_eq!(f.run(&["-c", &inc, "-c", "x.y=a", "config", "--get", "x.y"], &[]).0, "a\n");
    let (out, _, code) = f.run(&["-c", "x.y=a", "-c", &inc, "-c", "x.y=b", "config", "--list", "--show-scope"], &[]);
    assert_eq!(code, 0);
    let command: Vec<&str> = out.lines().filter(|l| l.starts_with("command\t")).collect();
    assert_eq!(
        command,
        [
            "command\tx.y=a".to_string(),
            format!("command\t{inc}"),
            "command\tx.y=inc".into(),
            "command\tinclude.path=inc2.cfg".into(),
            "command\tx.y=nested".into(),
            "command\tx.y=after".into(),
            "command\tx.y=b".into(),
        ]
    );
    // Outside any repository the walk is the same.
    let (out, _, _) = f.run(&["-C", "/", "-c", "x.y=a", "-c", &inc, "-c", "x.y=b", "config", "--get-all", "x.y"], &[]);
    assert_eq!(out, "a\ninc\nnested\nafter\nb\n");
}

#[test]
fn an_inherited_triple_is_walked_in_order() {
    let f = Fixture::new("env");
    let env = [
        ("GIT_CONFIG_COUNT", "3"),
        ("GIT_CONFIG_KEY_0", "x.y"),
        ("GIT_CONFIG_VALUE_0", "1"),
        ("GIT_CONFIG_KEY_1", "z.w"),
        ("GIT_CONFIG_VALUE_1", "2"),
        ("GIT_CONFIG_KEY_2", "x.y"),
        ("GIT_CONFIG_VALUE_2", "3"),
    ];
    let (out, err, code) = f.run(&["config", "--list", "--show-scope"], &env);
    assert_eq!((err.as_str(), code), ("", 0));
    assert!(out.starts_with(LOCAL), "{out}");
    assert!(out.ends_with("command\tx.y=1\ncommand\tz.w=2\ncommand\tx.y=3\n"), "{out}");

    let inc = f.root.join("inc.cfg").display().to_string();
    let env = [
        ("GIT_CONFIG_COUNT", "2"),
        ("GIT_CONFIG_KEY_0", "include.path"),
        ("GIT_CONFIG_VALUE_0", inc.as_str()),
        ("GIT_CONFIG_KEY_1", "x.y"),
        ("GIT_CONFIG_VALUE_1", "e"),
    ];
    assert_eq!(
        f.run(&["-c", "x.y=a", "-c", &f.inc(), "config", "--get-all", "x.y"], &env).0,
        "inc\nnested\nafter\ne\na\ninc\nnested\nafter\n"
    );
}
