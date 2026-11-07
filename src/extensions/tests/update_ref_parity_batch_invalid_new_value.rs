//! `git update-ref --stdin --batch-updates`: an invalid new value, and the
//! `HEAD` split that runs into an update already rejected.
//!
//! ref_transaction_update() refuses a nonexistent object, or a non-commit for a
//! branch, as REF_TRANSACTION_ERROR_INVALID_NEW_VALUE (refs.c:1433-1448), and
//! handle_ref_transaction_error() (builtin/update-ref.c:273-292) dies on it only
//! without `--batch-updates`; with it the update is reported at once, while its
//! command is parsed, and never joins the transaction. zvcs died 128 on both
//! backends.
//!
//! A rejected update's name leaves `transaction->refnames`
//! (ref_transaction_maybe_set_rejected(), refs.c:1276-1312), which the splits
//! search with a binary search over a list they append to unsorted
//! (string-list.c:17-40), while the lock it took stays: `update HEAD <A> <A>`
//! refused over the branch HEAD points at, then an update of that branch, adds
//! a log-only HEAD update that reuses HEAD's lock and so reads as no symref,
//! `HEAD has been racily updated` (refs/files-backend.c:2735-2757), a generic
//! error that dies. zvcs printed two `refname conflict` rejections. Under
//! `prepare` the same rejections are recorded rather than fatal, and the
//! `rejected` lines precede `commit: ok` on stdout.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::io::Write as _;
use std::process::{Command, Stdio};
use twin_repo::{Out, Side};

/// `bin <args>` in `side`'s repository, fed `input`.
fn run(bin: &str, side: &Side, args: &[&str], input: &str) -> Out {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(side.repo())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &side.root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    Out { code: out.status.code().unwrap_or(-1), stdout: text(&out.stdout), stderr: text(&out.stderr) }
}

/// `update-ref --stdin <flags>` fed `input`, then the refs and logs as stock reads them.
fn batch(side: &Side, stock: &str, flags: &[&str], input: &str) -> Vec<Out> {
    let mut args = vec!["update-ref", "--stdin"];
    args.extend_from_slice(flags);
    let mut seen = vec![run(&side.bin, side, &args, input)];
    for read in [
        &["for-each-ref"][..],
        &["reflog", "show", "--format=%H %gs", "HEAD"],
        &["reflog", "show", "--format=%H %gs", "main"],
        &["fsck"],
    ] {
        seen.push(run(stock, side, read, ""));
    }
    seen
}

fn compare(label: &str, format: &str, cases: &[(&[&str], &str)]) {
    let Some(stock) = stock_git::stock_git() else { return };
    for (n, (flags, template)) in cases.iter().enumerate() {
        let (s, z) = twin_repo::pair(&format!("{label}-{format}-{n}"), stock);
        for side in [&s, &z] {
            if format == "reftable" {
                run(stock, side, &["refs", "migrate", "--ref-format=reftable"], "");
            }
            run(stock, side, &["symbolic-ref", "refs/heads/sym", "refs/heads/main"], "");
        }
        let id = |spec: &str| s.git(&["rev-parse", spec]).stdout.trim().to_string();
        let input = template
            .replace("{A}", &id("main"))
            .replace("{B}", &id("main~1"))
            .replace("{T}", &id("main^{tree}"))
            .replace("{X}", "1111111111111111111111111111111111111111");
        assert_eq!(batch(&z, stock, flags, &input), batch(&s, stock, flags, &input), "{format} {flags:?} {input}");
    }
}

const BATCH: &[&str] = &["--batch-updates"];

#[test]
fn an_invalid_new_value_is_rejected_while_it_is_parsed() {
    let cases: &[(&[&str], &str)] = &[
        (BATCH, "update refs/heads/main {T}\n"),
        (BATCH, "update refs/heads/side {A} {X}\nupdate refs/heads/main {T} {A}\nupdate refs/heads/x {X}\ncreate refs/heads/y {T}\ncreate refs/tags/t {T}\nupdate HEAD {T}\n"),
        (BATCH, "update refs/heads/main {A} {B}\nupdate refs/heads/z {T} 0000000000000000000000000000000000000000\n"),
        (BATCH, "start\nupdate refs/heads/main {T}\nprepare\ncommit\n"),
        (&[], "update refs/heads/main {T}\n"),
    ];
    for format in ["files", "reftable"] {
        compare("update-ref-invalid-new", format, cases);
    }
}

#[test]
fn a_head_split_after_a_rejection() {
    let cases: &[(&[&str], &str)] = &[
        (BATCH, "update HEAD {A} {A}\nupdate refs/heads/main {A} {A}\n"),
        (BATCH, "update refs/heads/main {A} {A}\nupdate HEAD {A} {A}\n"),
        (BATCH, "update HEAD {B}\nupdate refs/heads/main {B}\n"),
        (BATCH, "update refs/heads/main {B}\nupdate refs/heads/sym {B}\nupdate HEAD {B}\n"),
        (BATCH, "update refs/heads/sym {B}\nupdate HEAD {B}\nupdate refs/heads/main {B}\n"),
        (BATCH, "start\nupdate HEAD {A} {A}\nupdate refs/heads/main {A} {A}\nprepare\ncommit\n"),
        (BATCH, "start\nupdate refs/heads/main {A} {A}\nupdate HEAD {A} {A}\nprepare\ncommit\n"),
        (BATCH, "start\nupdate refs/heads/main {B} {B}\nprepare\ncommit\n"),
        (&[], "update HEAD {A} {A}\nupdate refs/heads/main {A} {A}\n"),
    ];
    compare("update-ref-head-split", "files", cases);
}
