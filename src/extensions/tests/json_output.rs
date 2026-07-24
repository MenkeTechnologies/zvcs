//! Every read verb that advertises `--json` must emit valid JSON — one object per
//! line (NDJSON) — so scripts can rely on it. This runs the real binary against a
//! fixture repo and parses every line, so the guarantee can't silently drift as
//! verbs change. Self-contained: the fixture is built with the shadow binary
//! itself, so no system git is required.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(cwd: &Path, home: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run binary");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn read_verbs_emit_valid_json() {
    let root = std::env::temp_dir().join(format!("zvcs-jsonout-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    // Fixture repo, built with the shadow binary (add creates the index before the
    // first commit), then indexed so the machine-wide read verbs see it.
    run(&repo, &home, &["init", "-q", "-b", "main"]);
    run(&repo, &home, &["config", "user.email", "t@e.co"]);
    run(&repo, &home, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    run(&repo, &home, &["add", "f"]);
    run(&repo, &home, &["commit", "-q", "-m", "base"]);
    run(&repo, &home, &["tag", "v1"]);
    run(&repo, &home, &["zreindex", "--sync", repo.to_str().unwrap()]);

    // Each of these must print only valid JSON on stdout (empty is fine — no rows).
    let cases: &[&[&str]] = &[
        &["zverbs", "--json"],
        &["zrepos", "--json"],
        &["zheads", "--json"],
        &["ztags", "--json"],
        &["zbranches", "--json"],
        &["zremotes", "--json"],
        &["zsize", "--json"],
        &["zage", "--json"],
        &["zfiles", "--json"],
        &["zcommits", "--json"],
        &["zpristine", "--json"],
        &["zdirty", "--json"],
        &["zstatus", "--all", "--json"],
        &["zjobs", "--json"],
        &["zwho", "--json"],
        &["zsessions", "--json"],
        &["zcontend", "--json"],
        &["zgraph", "--json"],
        &["zdashboard", "--json"],
        &["zsince", "1h", "--json"],
        &["zahead", "--json"],
        &["zbehind", "--json"],
        &["zorphans", "--json"],
        &["zdivergent", "--json"],
        &["zpin", "list", "--json"],
        &["zppid", "--json"],
        &["zprocs", "--json"],
    ];
    for case in cases {
        let out = run(&repo, &home, case);
        for (i, line) in out.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|e| {
                panic!("`git {}` line {} is not JSON: {line:?} ({e})", case.join(" "), i + 1)
            });
        }
    }

    // At least one verb must actually produce a row (so `--json` isn't a silent
    // no-op): the indexed fixture repo shows up in `zrepos --json` with a `repo`.
    let repos = run(&repo, &home, &["zrepos", "--json"]);
    let first = repos.lines().find(|l| !l.trim().is_empty()).expect("zrepos --json produced no rows");
    let v: serde_json::Value = serde_json::from_str(first).unwrap();
    assert!(v.get("repo").is_some(), "zrepos --json row must carry a `repo` field: {first}");

    let _ = std::fs::remove_dir_all(&root);
}
