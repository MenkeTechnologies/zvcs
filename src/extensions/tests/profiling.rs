//! Profiling / multi-agent-view verbs: `zfiles`, `zlast`, `zorphans`, `zidle`,
//! `zsessions`, and the `zdashboard` aggregate. Builds a small indexed set and
//! asserts each verb's summary reflects the known state.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

fn zvcs(home: &Path, sock: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN).args(args).env("ZVCS_HOME", home).env("ZVCS_SOCK", sock).output().unwrap();
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

#[test]
fn profiling_and_dashboard_reflect_state() {
    let root = std::env::temp_dir().join(format!("zvcs-profiling-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let home = root.join("home");
    let sock = root.join("sock");

    // Two repos: alpha with two tracked files (one dirty), beta with one.
    let alpha = work.join("alpha");
    let beta = work.join("beta");
    std::fs::create_dir_all(&alpha).unwrap();
    std::fs::create_dir_all(&beta).unwrap();
    git(&alpha, &["init", "-q", "-b", "main"]);
    std::fs::write(alpha.join("a1"), "x\n").unwrap();
    std::fs::write(alpha.join("a2"), "y\n").unwrap();
    git(&alpha, &["add", "-A"]);
    git(&alpha, &["commit", "-qm", "c1"]);
    git(&beta, &["init", "-q", "-b", "main"]);
    std::fs::write(beta.join("b1"), "z\n").unwrap();
    git(&beta, &["add", "-A"]);
    git(&beta, &["commit", "-qm", "c1"]);
    std::fs::write(alpha.join("a1"), "x\ndirty\n").unwrap(); // alpha tracked-dirty

    assert!(zvcs(&home, &sock, &["zreindex", "--sync", work.to_str().unwrap()]).contains("indexed 2"));

    // zfiles: alpha has 2 tracked files, beta 1.
    let files = zvcs(&home, &sock, &["zfiles"]);
    assert!(files.lines().any(|l| l.contains("alpha") && l.contains("2 file(s)")), "zfiles alpha=2:\n{files}");
    assert!(files.lines().any(|l| l.contains("beta") && l.contains("1 file(s)")), "zfiles beta=1:\n{files}");

    // zorphans: neither has a remote → both listed.
    let orphans = zvcs(&home, &sock, &["zorphans"]);
    assert!(orphans.contains("2 of 2 indexed have no remote"), "zorphans: {orphans}");

    // zlast: both repos appear (order is by commit time).
    let last = zvcs(&home, &sock, &["zlast"]);
    assert!(last.contains("alpha") && last.contains("beta"), "zlast lists both:\n{last}");

    // zsessions with no claims; zidle lists all unclaimed.
    assert!(zvcs(&home, &sock, &["zsessions"]).contains("no active claims"), "zsessions empty");
    let idle = zvcs(&home, &sock, &["zidle"]);
    assert!(idle.contains("2 of 2 indexed unclaimed"), "zidle: {idle}");

    // zdashboard is cache-based (instant), so with no daemon the status cache is
    // cold: it reports the repo count and the coverage note, not live-walked
    // counts. This is the whole point — it must not walk 3000+ repos live.
    let dash = zvcs(&home, &sock, &["zdashboard"]);
    assert!(dash.contains("2 repos indexed"), "dashboard header: {dash}");
    assert!(dash.lines().any(|l| l.trim_start().starts_with("dirty")), "dashboard has a dirty line:\n{dash}");
    assert!(dash.contains("status cached for 0/2"), "cold cache note when no daemon:\n{dash}");

    let _ = std::fs::remove_dir_all(&root);
}
