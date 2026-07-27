//! The progress meter a pack write reports, and the terminal check that gates it.
//!
//! git writes `Enumerating objects`, `Counting objects`, `Compressing objects`,
//! `Writing objects` and the closing `Total …` to stderr while it builds a pack,
//! redrawing each line in place with a carriage return and ending the phase with
//! `, done.` and a newline. `--progress` forces the meter on; without it the
//! meter appears only on a terminal, so a piped `gc` or `repack` is silent.
//!
//! The regression this pins: `gc` reported nothing at all, on a terminal or off
//! one, because nothing enabled the meter.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn cmd(dir: &Path, args: &[&str]) -> Command {
    let home = dir.join(".isolated-home");
    std::fs::create_dir_all(&home).unwrap();
    let mut c = Command::new(BIN);
    c.args(args)
        .current_dir(dir)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
    c
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = cmd(dir, args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fixture(tag: &str) -> PathBuf {
    let repo = std::env::temp_dir().join(format!("zvcs-progress-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    for i in 0..3 {
        std::fs::write(repo.join(format!("f{i}")), format!("v{i}\n")).unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", &format!("c{i}")]);
    }
    repo
}

/// `pack-objects` reading its object list on stdin, with the meter forced on so
/// the assertions do not need a terminal.
fn pack_objects_with_progress(repo: &Path) -> Output {
    let list = git(repo, &["rev-list", "--all", "--objects"]);
    let ids: String = list
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(|id| format!("{id}\n"))
        .collect();

    let mut child = cmd(repo, &["pack-objects", "--progress", "--stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(ids.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn every_phase_reports_in_gits_order_and_framing() {
    let repo = fixture("phases");
    let out = pack_objects_with_progress(&repo);
    assert!(out.status.success(), "pack-objects failed");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();

    // The object count is whatever the traversal found; every phase has to agree
    // on it, which is what makes the closing lines checkable against each other.
    let total: usize = err
        .split("Enumerating objects: ")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or_else(|| panic!("no enumeration line in:\n{err}"));
    assert!(total > 3, "fixture should hold more than three objects, got {total}");

    for expected in [
        format!("Enumerating objects: {total}, done.\n"),
        format!("Counting objects: 100% ({total}/{total}), done.\n"),
        format!("Writing objects: 100% ({total}/{total}), done.\n"),
    ] {
        assert!(err.contains(&expected), "missing {expected:?} in:\n{err}");
    }
    assert!(
        err.contains("Delta compression using up to "),
        "the delta search announces its thread count:\n{err}"
    );
    assert!(
        err.contains(&format!("Total {total} (delta ")),
        "the closing summary counts every object written:\n{err}"
    );

    // Redraws overwrite in place: the line before a phase's closing line ends in
    // a carriage return, never a newline.
    let closing = format!("Writing objects: 100% ({total}/{total})\rWriting objects: 100%");
    assert!(err.contains(&closing), "redraws must end in a carriage return:\n{err}");

    // Phase order, as git emits it.
    let at = |needle: &str| err.find(needle).unwrap_or_else(|| panic!("no {needle:?}"));
    assert!(at("Enumerating objects") < at("Counting objects"));
    assert!(at("Counting objects") < at("Compressing objects"));
    assert!(at("Compressing objects") < at("Writing objects"));
    assert!(at("Writing objects") < at("Total "));
}

/// The meter is a terminal courtesy: piped, every one of these stays silent, so
/// a script capturing `gc` sees exactly what it did before.
#[test]
fn a_piped_run_reports_nothing() {
    let repo = fixture("piped");
    for args in [vec!["gc"], vec!["repack", "-ad"], vec!["pack-objects", "--stdout"]] {
        let out = match args[0] {
            "pack-objects" => {
                let mut child = cmd(&repo, &args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                child.wait_with_output().unwrap()
            }
            _ => cmd(&repo, &args).output().unwrap(),
        };
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "",
            "{args:?} wrote progress to a pipe"
        );
    }
}

/// `-q` suppresses the meter even where it would otherwise be on, and `gc`
/// forwards that to the pack write rather than dropping it.
#[test]
fn quiet_suppresses_the_meter() {
    let repo = fixture("quiet");
    let list = git(&repo, &["rev-list", "--all", "--objects"]);
    let ids: String = list
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(|id| format!("{id}\n"))
        .collect();

    let mut child = cmd(&repo, &["pack-objects", "--progress", "-q", "--stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(ids.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "",
        "-q wins over --progress, being the later flag"
    );
}
