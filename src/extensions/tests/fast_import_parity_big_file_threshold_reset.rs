//! `git fast-import --big-file-threshold` and `option git big-file-threshold`
//! after the object database is first prepared.
//!
//! Both set `repo->settings.big_file_threshold`, but the first
//! prepare_repo_settings() of the process opens with repo_settings_clear()
//! (repo-settings.c:30-46, :160-166), and fast-import reaches it only through
//! prepare_multi_pack_index_one() (midx.c:741-745) on the first pack lookup:
//! store_object() and stream_blob() looking for an object the run has not seen,
//! or any object read. From then on repo_settings_get_big_file_threshold() is
//! `core.bigFileThreshold` or 512 MiB again, so only the blobs ahead of that
//! point are streamed. zvcs kept the command-line value for the whole run, which
//! streamed every later blob and named the pack differently. `cat-blob` with
//! anything after the full hex is `garbage after SHA1` (builtin/fast-import.c:
//! 3456-3460), where zvcs called it an invalid dataref.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::io::Write as _;
use std::process::{Command, Stdio};
use twin_repo::{Out, Side};

fn import(side: &Side, args: &[&str], stream: &str) -> (Out, Vec<String>) {
    let mut child = Command::new(&side.bin)
        .args(["fast-import", "--quiet"])
        .args(args)
        .current_dir(side.repo())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &side.root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stream.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    // The crash report is named after the pid.
    let stderr = text(&out.stderr);
    let stderr = match stderr.find("fast_import_crash_") {
        Some(at) => stderr[..at].to_string(),
        None => stderr,
    };
    let mut packs: Vec<String> = std::fs::read_dir(side.repo().join(".git/objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    packs.sort();
    (Out { code: out.status.code().unwrap_or(-1), stdout: text(&out.stdout), stderr }, packs)
}

/// Four 520-byte blobs that delta well against each other, and a commit using two.
fn stream(head: &str) -> String {
    let body = "line of text that repeats\n".repeat(20);
    let mut s = head.to_string();
    for (mark, tail) in [(1, "a"), (2, "b"), (3, "c"), (4, "d")] {
        let data = format!("{body}{tail}\n");
        s.push_str(&format!("blob\nmark :{mark}\ndata {}\n{data}\n", data.len()));
    }
    s.push_str("commit refs/heads/imported\ncommitter C <c@x> 1700000000 +0000\ndata 2\nm\nM 100644 :2 f\nM 100644 :3 g\n\n");
    s
}

#[test]
fn the_threshold_falls_back_once_the_object_database_is_prepared() {
    let Some(stock) = stock_git::stock_git() else { return };
    let tiny = "blob\nmark :9\ndata 5\ntiny\n\n";
    for (n, (config, args, head)) in [
        (None, &["--big-file-threshold=100"][..], String::new()),
        (None, &["--big-file-threshold=100"], tiny.to_string()),
        (None, &[], "option git big-file-threshold=100\n".to_string()),
        (Some("200"), &["--big-file-threshold=100"], String::new()),
        (Some("100"), &["--big-file-threshold=0"], String::new()),
        (Some("100"), &[], String::new()),
        (None, &["--big-file-threshold=100"], "commit refs/heads/x\ncommitter C <c@x> 1700000000 +0000\ndata 2\nm\nfrom main^0\n\n".to_string()),
        (None, &["--big-file-threshold=100"], "ls main a\n".to_string()),
        (None, &[], "cat-blob 0123456789012345678901234567890123456789:a\n".to_string()),
    ]
    .into_iter()
    .enumerate()
    {
        let (s, z) = twin_repo::pair(&format!("fast-import-big-file-{n}"), stock);
        for side in [&s, &z] {
            side.git(&["repack", "-qad"]);
            side.git(&["config", "fastimport.unpackLimit", "1"]);
            if let Some(threshold) = config {
                side.git(&["config", "core.bigFileThreshold", threshold]);
            }
        }
        let input = stream(&head);
        assert_eq!(import(&z, args, &input), import(&s, args, &input), "{config:?} {args:?} {head}");
    }
}
