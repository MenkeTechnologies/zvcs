//! `git gc` honors `gc.maxCruftSize` as the default for `--max-cruft-size`.
//!
//! The value tunes the size at which git splits cruft packs; this port applies
//! no size limit, so its one observable effect is git's `warning: minimum pack
//! size limit is 1 MiB` for a non-zero value below that floor. The config feeds
//! exactly the same warning path the `--max-cruft-size` flag already drove, and
//! is validated the moment the config is read — a value git's `git_config_ulong`
//! rejects is fatal (exit 128) even under a `--max-cruft-size` override or a
//! below-threshold `--auto`, and only a bare `gc -h` escapes it.
//!
//! Every case is checked against the system `git` (2.55.0) run with a
//! byte-identical environment. A valid `gc` run cannot be compared on full
//! stderr because this port's `rerere gc` delegate emits an unrelated usage
//! block there; the assertions target the size-limit warning line and the exit
//! code, which is the signal `gc.maxCruftSize` controls. A rejected value dies
//! before that delegate runs, so those diagnostics *are* compared verbatim.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const WARNING: &str = "warning: minimum pack size limit is 1 MiB";

/// Run a system-`git` command in `dir`, asserting success. Used only to build
/// the fixture and to write `.git/config`, never as the behavior under test.
fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A one-commit repository plus an isolated, empty `HOME`, so no ambient global
/// `gc.*` config leaks into the run.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-gccfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    std::fs::write(repo.join("f"), "hello\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    (repo, home)
}

/// Run `git gc [extra]` under a deterministic, isolated environment. `bin` is
/// either the zvcs binary or the system `git`, run with byte-identical env so
/// their outputs are directly comparable.
fn run_gc(bin: &str, repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["gc"];
    args.extend_from_slice(extra);
    Command::new(bin)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn zvcs(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    run_gc(BIN, repo, home, extra)
}

fn real(repo: &Path, home: &Path, extra: &[&str]) -> Output {
    run_gc("git", repo, home, extra)
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Whether the run emitted git's below-floor warning, matched as a full line so
/// the unrelated `rerere` usage block on the same stream cannot false-positive.
fn warned(o: &Output) -> bool {
    stderr(o).lines().any(|l| l == WARNING)
}

#[test]
fn gc_max_cruft_size_config_default_and_warning() {
    let (repo, home) = fixture("warn");

    // A non-zero value below git's 1 MiB floor warns, and the port agrees with
    // git on that.
    git(&repo, &["config", "gc.maxCruftSize", "1024"]);
    let z = zvcs(&repo, &home, &[]);
    assert!(z.status.success(), "gc must still succeed:\n{}", stderr(&z));
    assert!(warned(&z), "gc.maxCruftSize=1024 must warn:\n{}", stderr(&z));
    assert!(warned(&real(&repo, &home, &[])), "sanity: git also warns");

    // At the floor the warning stops; `2m` is exactly 2 MiB.
    git(&repo, &["config", "gc.maxCruftSize", "2m"]);
    let z = zvcs(&repo, &home, &[]);
    assert!(z.status.success());
    assert!(!warned(&z), "2m clears the floor, so no warning:\n{}", stderr(&z));
    assert!(!warned(&real(&repo, &home, &[])), "sanity: git is silent at 2m");

    // Zero means "no limit" and is silent.
    git(&repo, &["config", "gc.maxCruftSize", "0"]);
    assert!(!warned(&zvcs(&repo, &home, &[])), "0 is unlimited, no warning");

    // `--max-cruft-size` overrides the config value, warning and all.
    git(&repo, &["config", "gc.maxCruftSize", "1024"]);
    assert!(
        !warned(&zvcs(&repo, &home, &["--max-cruft-size=2m"])),
        "CLI --max-cruft-size=2m must override the small config value"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn gc_max_cruft_size_base0_parsing_matches_git() {
    // git parses the value base-0 with an optional k/m/g suffix, so `0x400` is
    // hex 1024 and `010` is octal 8 (both below the floor → warn), `0k` is zero
    // (silent), and `1g` clears the floor (silent). The port must agree on each.
    for (value, expect_warn) in [("0x400", true), ("010", true), ("0k", false), ("1g", false)] {
        let (repo, home) = fixture(&format!("base0-{value}"));
        git(&repo, &["config", "gc.maxCruftSize", value]);
        let z = zvcs(&repo, &home, &[]);
        assert!(z.status.success(), "gc.maxCruftSize={value} must be accepted:\n{}", stderr(&z));
        assert_eq!(
            warned(&z),
            expect_warn,
            "gc.maxCruftSize={value} warning expectation:\n{}",
            stderr(&z)
        );
        assert_eq!(
            warned(&z),
            warned(&real(&repo, &home, &[])),
            "gc.maxCruftSize={value} must match git's warn/no-warn"
        );
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }
}

#[test]
fn gc_max_cruft_size_invalid_config_is_fatal() {
    // Each rejected value: exit 128, and the diagnostic byte-for-byte with git
    // — including the ` in file .git/config` origin clause. The run dies before
    // the `rerere` delegate, so the whole stderr is just this line.
    let cases = [
        ("bogus", "invalid unit"),
        ("-1", "invalid unit"),
        ("1x", "invalid unit"),
        ("1.5", "invalid unit"),
        ("999999999999999999999999", "out of range"),
    ];
    for (i, (value, reason)) in cases.iter().enumerate() {
        let (repo, home) = fixture(&format!("bad{i}"));
        git(&repo, &["config", "gc.maxCruftSize", value]);

        let z = zvcs(&repo, &home, &[]);
        let g = real(&repo, &home, &[]);
        assert_eq!(z.status.code(), Some(128), "gc.maxCruftSize={value} must exit 128");
        assert_eq!(g.status.code(), Some(128), "sanity: git exits 128 for {value}");
        assert_eq!(
            stderr(&z),
            stderr(&g),
            "gc.maxCruftSize={value} diagnostic must match git byte-for-byte"
        );
        assert_eq!(
            stderr(&z),
            format!(
                "fatal: bad numeric config value '{value}' for 'gc.maxcruftsize' in file .git/config: {reason}\n"
            ),
            "unexpected diagnostic text for {value}"
        );

        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }
}

#[test]
fn gc_max_cruft_size_validated_before_override_and_auto() {
    // git validates the config the moment it reads it, so a bad value is fatal
    // even when `--max-cruft-size` overrides it or a below-threshold `--auto`
    // would otherwise decline to run.
    let (repo, home) = fixture("eager");
    git(&repo, &["config", "gc.maxCruftSize", "bogus"]);

    let z = zvcs(&repo, &home, &["--max-cruft-size=2m"]);
    assert_eq!(z.status.code(), Some(128), "override must not rescue a bad config");
    assert_eq!(stderr(&z), stderr(&real(&repo, &home, &["--max-cruft-size=2m"])));

    let z = zvcs(&repo, &home, &["--auto"]);
    assert_eq!(z.status.code(), Some(128), "--auto must not skip config validation");
    assert_eq!(stderr(&z), stderr(&real(&repo, &home, &["--auto"])));

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn gc_dash_h_skips_config_validation() {
    // A bare `gc -h` prints usage and exits 129 before the config is read, so an
    // otherwise-fatal `gc.maxCruftSize` is never seen — matching git's early
    // `-h` fast path.
    let (repo, home) = fixture("dashh");
    git(&repo, &["config", "gc.maxCruftSize", "bogus"]);

    let z = zvcs(&repo, &home, &["-h"]);
    assert_eq!(z.status.code(), Some(129), "bare -h exits 129");
    assert_eq!(real(&repo, &home, &["-h"]).status.code(), Some(129), "sanity: git too");
    assert!(
        String::from_utf8_lossy(&z.stdout).starts_with("usage: git gc"),
        "usage goes to stdout"
    );
    assert!(
        !stderr(&z).contains("bad numeric config value"),
        "the bad config must not be reported under -h:\n{}",
        stderr(&z)
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
