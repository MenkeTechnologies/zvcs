//! `git zsubmit` ships an arbitrary command to the daemon's worker pool and
//! tracks it via the job ledger. This exercises the inline path (no daemon): the
//! command runs, produces its effect and output, and shows up in `zjobs`/`zjob`
//! with its command as the label. A `zdaemon.disabled` marker keeps a daemon from
//! autostarting so the run stays inline and deterministic.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").args(args).current_dir(dir).status().unwrap().success(), "git {args:?}");
}

fn zvcs_in(repo: &Path, home: &Path, sock: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("ZVCS_HOME", home)
        .env("ZVCS_SOCK", sock)
        .output()
        .unwrap();
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

#[test]
fn zsubmit_runs_and_records_the_job() {
    let root = std::env::temp_dir().join(format!("zvcs-zsubmit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let sock = std::env::temp_dir().join(format!("zsub-{}.sock", std::process::id()));
    // Keep any daemon from autostarting, so zsubmit runs the job inline.
    std::fs::write(home.join("zdaemon.disabled"), "").unwrap();

    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f"), "hi\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["-c", "user.email=t@e.x", "-c", "user.name=t", "commit", "-qm", "c1"]);

    // Ship a command that writes a file and prints to stdout.
    let submit = zvcs_in(&repo, &home, &sock, &["zsubmit", "--", "sh", "-c", "echo made > out.txt; printf done-output"]);
    assert!(submit.contains("job #"), "zsubmit should report a job id: {submit}");

    // Its effect happened (inline path runs synchronously).
    assert_eq!(std::fs::read_to_string(repo.join("out.txt")).unwrap(), "made\n", "command wrote its file");

    // The ledger shows it, labeled with the command, and its output is captured.
    let jobs = zvcs_in(&repo, &home, &sock, &["zjobs"]);
    assert!(jobs.contains("exec: sh -c"), "zjobs shows the command label:\n{jobs}");
    let id: String = jobs.chars().skip_while(|c| *c != '#').skip(1).take_while(|c| c.is_ascii_digit()).collect();
    assert!(!id.is_empty(), "a job id is listed:\n{jobs}");
    let detail = zvcs_in(&repo, &home, &sock, &["zjob", &id]);
    assert!(detail.contains("done-output"), "zjob shows captured output:\n{detail}");

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&sock);
}
