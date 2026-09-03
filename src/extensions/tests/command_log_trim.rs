//! The fleet command log across a trim.
//!
//! Every `git` invocation appends one line to `$ZVCS_HOME/commands.log` while
//! `git zcommands` logging is on, and `zcommands` and `zaudit` answer "what ran,
//! where, by whom" out of that file. Past a size cap the writer trims it: read
//! the file, keep the newest lines, write them back. That read-modify-write
//! races every other command's append, and the append loses — a line written
//! between the trim's read and its write is erased by a rewrite that never saw
//! it, while the command that wrote it exited 0.
//!
//! Measured before the fix: sixteen commands run at once over an oversized log,
//! thirteen of their entries in the trimmed file. An audit trail that quietly
//! drops entries is worse than none, because it is read as complete.
//!
//! Both sides of the race take the log's lock now — the append as well as the
//! trim — since an `O_APPEND` write that lands whole is still erased by a
//! rewrite that never saw it.
//!
//! The fixture pushes the log past the cap with filler rather than by running
//! millions of commands, so the trim is guaranteed to fire during the burst.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");
const AGENTS: usize = 16;
/// Well above `zcommands`'s 5 MiB cap. Size is what makes the failure likely:
/// the trim reads the whole file and writes the tail back, so a bigger log is a
/// longer window for a concurrent append to fall into. Measured against an
/// unlocked trim, entries were lost on one run in three at 160_000 lines and two
/// in three here — the detection is a probability, not a certainty, while the
/// locked path passes every time. A failure is therefore always real; a pass is
/// not by itself proof the lock is still there.
const FILLER_LINES: usize = 700_000;

fn run(home: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap()
}

fn ok(home: &Path, dir: &Path, args: &[&str]) -> String {
    let out = run(home, dir, args);
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_trim_does_not_erase_entries_written_while_it_runs() {
    let root = std::env::temp_dir().join(format!("zvcs-cmdlog-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    ok(&home, &repo, &["init", "-q", "-b", "main"]);
    ok(&home, &repo, &["config", "user.email", "t@example"]);
    ok(&home, &repo, &["config", "user.name", "T"]);
    ok(&home, &repo, &["commit", "-q", "--allow-empty", "-m", "c0"]);

    // `zcommands on` enables logging and then follows the feed, so it needs
    // `--no-follow` or it never returns.
    let enabled = run(&home, &repo, &["zcommands", "on", "--no-follow"]);
    assert!(enabled.status.success(), "could not enable command logging");
    // The file appears with the first logged command, not with the switch.
    ok(&home, &repo, &["rev-parse", "HEAD"]);
    let log = home.join("commands.log");
    assert!(log.exists(), "no command log after logging was enabled and a command ran");

    // Push it past the cap so the burst below trims while it appends.
    {
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        let filler = "1700000000\t1\t1\t/x\tfiller line here\n".repeat(1000);
        for _ in 0..(FILLER_LINES / 1000) {
            f.write_all(filler.as_bytes()).unwrap();
        }
    }
    let before = std::fs::metadata(&log).unwrap().len();
    assert!(before > 5 * 1024 * 1024, "the fixture must exceed the trim cap, got {before} bytes");

    // A burst of commands, each of which appends one line and may trim.
    let kids: Vec<Child> = (0..AGENTS)
        .map(|_| {
            Command::new(BIN)
                .args(["rev-parse", "--short", "HEAD"])
                .current_dir(&repo)
                .env("HOME", &home)
                .env("ZVCS_HOME", &home)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("spawn command")
        })
        .collect();
    for mut k in kids {
        assert!(k.wait().expect("command exited").success(), "a logged command failed");
    }

    let text = std::fs::read_to_string(&log).unwrap();
    let trimmed = std::fs::metadata(&log).unwrap().len();
    assert!(trimmed < before, "the log was never trimmed, so this proves nothing ({trimmed} bytes)");

    // Every command that ran is in the trail. These are the newest lines in the
    // file, so the trim's own line budget cannot be the reason one is missing.
    let kept = text.lines().filter(|l| l.contains("rev-parse")).count();
    assert_eq!(kept, AGENTS, "a trim erased {} entries that were written while it ran", AGENTS - kept);

    // And nothing was interleaved into a half-line.
    for (i, line) in text.lines().enumerate() {
        assert_eq!(
            line.split('\t').count(),
            5,
            "line {} of the log is malformed: {line:?}",
            i + 1
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}
