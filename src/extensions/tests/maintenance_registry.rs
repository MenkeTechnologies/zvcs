//! `git maintenance register` / `unregister` and the `maintenance.repo` registry.
//!
//! The two subcommands do not read the registry from the file they write to.
//! `register` asks `repo_config_get_string_multi(the_repository, ...)`
//! (builtin/gc.c:2127) — the *merged* configuration, `--config-file` or not —
//! so a repository already listed anywhere is not appended again. `unregister`
//! asks that same merged configuration, unless `--config-file` was given, in
//! which case it asks that one file through a config set of its own
//! (builtin/gc.c:2190-2196). The removal, meanwhile, always targets the global
//! file (or `--config-file`). The two can therefore disagree, and git's answer to
//! the disagreement is the `CONFIG_NOTHING_SET` branch at gc.c:2220-2223, which
//! `--force` deliberately does not swallow.
//!
//! A valueless `maintenance.repo` entry fails the lookup outright:
//! `check_multi_string()` (config.c:1873-1890) prints
//! `error: missing value for 'maintenance.repo'` and returns -1, which both
//! callers read as "not listed". It is an `error:`, not a `die()`, so it changes
//! no exit code by itself.
//!
//! Measured against git 2.55.0; the expectations below are stock's bytes.

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A repository plus an empty global config file, fully isolated from the
/// developer's own configuration.
struct Fixture {
    repo: PathBuf,
    global: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-maintreg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(
            Command::new(BIN)
                .args(["init", "-q", "-b", "main"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success(),
            "init failed"
        );
        let global = root.join("global-config");
        std::fs::write(&global, "").unwrap();
        Fixture { repo, global }
    }

    /// The realpath git records in `maintenance.repo` for this repository.
    fn maintpath(&self) -> String {
        self.repo.canonicalize().unwrap().to_str().unwrap().to_owned()
    }

    /// Overwrite the repository's own config, keeping it a valid repository.
    fn set_local(&self, body: &str) {
        std::fs::write(
            self.repo.join(".git/config"),
            format!("[core]\n\trepositoryformatversion = 0\n{body}"),
        )
        .unwrap();
    }

    fn global_text(&self) -> String {
        std::fs::read_to_string(&self.global).unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg("maintenance")
            .args(args)
            .current_dir(&self.repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &self.global)
            .env("HOME", self.repo.parent().unwrap())
            .env("ZVCS_HOME", self.repo.parent().unwrap())
            .output()
            .unwrap()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A repository already listed in its *local* config is registered nowhere else:
/// the membership test is on the merged configuration, so the global file is left
/// untouched even though it is the file `register` would have written to.
#[test]
fn register_consults_the_merged_config_not_the_file_it_writes() {
    let fx = Fixture::new("merged");
    fx.set_local(&format!("[maintenance]\n\trepo = {}\n", fx.maintpath()));

    let out = fx.run(&["register"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        fx.global_text(),
        "",
        "an already-listed repository was appended to the global config anyway"
    );

    // The control: with nothing listed, the same command does append.
    let fresh = Fixture::new("merged-control");
    assert_eq!(fresh.run(&["register"]).status.code(), Some(0));
    assert!(
        fresh.global_text().contains(&format!("repo = {}", fresh.maintpath())),
        "register wrote nothing: {:?}",
        fresh.global_text()
    );
}

/// `unregister` finds the repository in the local config, then cannot remove it
/// from the global file. `set_multivar_in_file` answers `CONFIG_NOTHING_SET`, and
/// gc.c:2220-2223 turns that into `fatal: unable to unset ...` — a *different*
/// message from the "not registered" one, and one `--force` does not suppress,
/// because the condition is `rc && (!force || rc == CONFIG_NOTHING_SET)`.
#[test]
fn unregister_refuses_when_the_match_is_not_in_the_file_it_writes() {
    let fx = Fixture::new("nothingset");
    let want = format!(
        "fatal: unable to unset 'maintenance.repo' value of '{}'\n",
        fx.maintpath()
    );

    for args in [&["unregister"][..], &["unregister", "--force"][..]] {
        fx.set_local(&format!("[maintenance]\n\trepo = {}\n", fx.maintpath()));
        let out = fx.run(args);
        assert_eq!(out.status.code(), Some(128), "{args:?} -> {out:?}");
        assert_eq!(stderr(&out), want, "{args:?}");
    }
}

/// The two refusals must not be conflated. A repository listed *nowhere* never
/// reaches the write at all: it is `fatal: repository '<path>' is not registered`
/// at 128, and there `--force` does make it a silent success.
#[test]
fn an_unlisted_repository_gets_the_other_refusal() {
    let fx = Fixture::new("unlisted");

    let plain = fx.run(&["unregister"]);
    assert_eq!(plain.status.code(), Some(128), "{plain:?}");
    assert_eq!(
        stderr(&plain),
        format!("fatal: repository '{}' is not registered\n", fx.maintpath())
    );

    let forced = fx.run(&["unregister", "--force"]);
    assert_eq!(forced.status.code(), Some(0), "{forced:?}");
    assert!(forced.stderr.is_empty(), "{forced:?}");
}

/// A `maintenance.repo` written with no `=` fails the whole lookup with an
/// `error:` line that changes no exit code. `register` therefore adds the
/// repository as if it were not listed, and `unregister` refuses as if it were
/// not listed — the same `error:` line preceding each outcome.
#[test]
fn a_valueless_entry_fails_the_lookup_without_failing_the_command() {
    let valueless = "[maintenance]\n\trepo\n";
    let missing = "error: missing value for 'maintenance.repo'\n";

    let reg = Fixture::new("valueless-reg");
    reg.set_local(valueless);
    let out = reg.run(&["register"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stderr(&out), missing);
    assert!(
        reg.global_text().contains(&format!("repo = {}", reg.maintpath())),
        "the failed lookup should read as 'not listed': {:?}",
        reg.global_text()
    );

    let unreg = Fixture::new("valueless-unreg");
    unreg.set_local(valueless);
    let out = unreg.run(&["unregister"]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        stderr(&out),
        format!(
            "{missing}fatal: repository '{}' is not registered\n",
            unreg.maintpath()
        )
    );
}

/// The registry is rewritten through git's own config writer, so removing the
/// entry removes its whole line — and the `[maintenance]` section with it when
/// nothing else is left. A leftover header or a stray indent is a real difference:
/// the next `git config` read of that file sees a section that git would not have
/// written.
#[test]
fn removing_an_entry_leaves_no_debris() {
    let fx = Fixture::new("debris");
    let cfg = fx.repo.parent().unwrap().join("registry");

    // Sole key: the section goes too, leaving an empty file.
    std::fs::write(&cfg, format!("[maintenance]\n\trepo = {}\n", fx.maintpath())).unwrap();
    let lone = fx.run(&["unregister", "--config-file", cfg.to_str().unwrap()]);
    assert_eq!(lone.status.code(), Some(0), "{lone:?}");
    assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "");

    // A sibling key keeps the section, and the removed line leaves no blank
    // behind it.
    std::fs::write(
        &cfg,
        format!("[maintenance]\n\trepo = {}\n\tauto = false\n", fx.maintpath()),
    )
    .unwrap();
    let sibling = fx.run(&["unregister", "--config-file", cfg.to_str().unwrap()]);
    assert_eq!(sibling.status.code(), Some(0), "{sibling:?}");
    assert_eq!(
        std::fs::read_to_string(&cfg).unwrap(),
        "[maintenance]\n\tauto = false\n"
    );
}

/// `register`'s own writes into the repository config are git's too: a
/// `[maintenance]` section that already exists gains `\tauto = false` with the
/// spaces around the `=`, not the bare `auto=false` a section editor would emit.
#[test]
fn register_writes_local_keys_in_gits_spelling() {
    let fx = Fixture::new("spelling");
    fx.set_local("[maintenance]\n\tstrategy = geometric\n");

    let out = fx.run(&["register"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let local = std::fs::read_to_string(fx.repo.join(".git/config")).unwrap();
    assert!(local.contains("\tauto = false\n"), "{local:?}");
    // `maintenance.strategy` already had a value, so it is left alone.
    assert!(local.contains("\tstrategy = geometric\n"), "{local:?}");
    assert!(!local.contains("incremental"), "{local:?}");
}

/// `register` then `unregister` returns the global config to exactly what it was.
#[test]
fn register_and_unregister_round_trip() {
    let fx = Fixture::new("roundtrip");
    assert_eq!(fx.run(&["register"]).status.code(), Some(0));
    assert!(fx.global_text().contains("[maintenance]"), "{:?}", fx.global_text());
    assert_eq!(fx.run(&["unregister"]).status.code(), Some(0));
    assert_eq!(fx.global_text(), "");
}

