//! Thin packs: `push` is thin unless `--no-thin` (builtin/push.c:59, :380), a
//! push through the smart-HTTP helper always is (remote-curl.c:1375-1376, :1573),
//! and `send-pack` only with `--thin`. Each spawns `pack-objects --revs --stdout
//! --thin` fed `<want>… --not <have>…` (send-pack.c:61-115,
//! odb/source-files.c:825-874), whose `--objects-edge` walk makes the edge
//! commits' trees preferred bases (builtin/pack-objects.c:4494-4497, :5323-5327),
//! so a delta may name a base only the receiver holds and `index-pack --fix-thin`
//! appends it to the kept pack.
//!
//! The source repository is repacked first, so the preferred bases are stored as
//! deltas there: `check_object()` reuses no delta of a preferred base
//! (builtin/pack-objects.c:2304, :2336), which is what keeps it a whole candidate
//! in the delta window. The server side is stock `receive-pack` (or stock
//! `http-backend`) for both clients with `receive.unpackLimit=1`, so every
//! object the server keeps is one the client sent or one `--fix-thin` added.
//! Stock git is the oracle (`support/stock_git.rs`).

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

#[path = "support/cgi_server.rs"]
mod cgi_server;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git_in(bin: &str, cwd: &Path, root: &Path, stdin: &[u8], args: &[&str]) -> std::process::Output {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

fn git(bin: &str, cwd: &Path, root: &Path, args: &[&str]) -> std::process::Output {
    git_in(bin, cwd, root, b"", args)
}

/// `src` has `one`, `two`, then `three`/`four` on `main` and `side1` (tagged
/// `vside`) on `side`, merged into `main`; `srv/s.git` was cloned at `two`.
/// Every file is long enough that each later version deltifies against the
/// one in `two`.
fn fixture(stock: &str, tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("zvcs-pushthin-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("srv")).unwrap();
    let root = root.canonicalize().unwrap();
    let src = root.join("src");
    let ok = |cwd: &Path, date: u64, args: &[&str]| {
        let stamp = format!("{date} +0000");
        let mut full = vec!["-c", "core.abbrev=10"];
        full.extend_from_slice(args);
        let out = Command::new(stock)
            .args(&full)
            .current_dir(cwd)
            .env("HOME", &root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .env("GIT_AUTHOR_DATE", &stamp)
            .env("GIT_COMMITTER_DATE", &stamp)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    let write = |name: &str, body: String| std::fs::write(src.join(name), body).unwrap();
    let lines = |f: &dyn Fn(u32) -> String, n: u32| (1..=n).map(f).collect::<String>();
    let big = |edits: &[(u32, &str)]| {
        lines(
            &|i| match edits.iter().find(|(at, _)| *at == i) {
                Some((_, w)) => format!("line {w} of a fairly long file used to make deltas worthwhile\n"),
                None => format!("line {i} of a fairly long file used to make deltas worthwhile\n"),
            },
            400,
        )
    };
    ok(&root, 1_700_000_000, &["init", "-q", "-b", "main", "src"]);
    std::fs::create_dir_all(src.join("sub")).unwrap();
    write("big.txt", big(&[]));
    write("other.txt", lines(&|i| format!("other {i} content for the second file\n"), 300));
    write("sub/s.txt", lines(&|i| format!("sub {i}\n"), 200));
    ok(&src, 1_700_000_000, &["add", "."]);
    ok(&src, 1_700_000_000, &["commit", "-qm", "one"]);
    write("big.txt", big(&[(7, "seven")]));
    ok(&src, 1_700_000_100, &["commit", "-qam", "two"]);
    ok(&root, 1_700_000_100, &["clone", "-q", "--bare", "src", "srv/s.git"]);
    let srv = root.join("srv/s.git");
    ok(&srv, 0, &["config", "receive.unpackLimit", "1"]);
    ok(&srv, 0, &["config", "receive.autogc", "false"]);
    ok(&srv, 0, &["config", "http.receivepack", "true"]);
    write("big.txt", big(&[(7, "seven"), (200, "two hundred")]));
    write("other.txt", lines(&|i| if i == 5 { "other five\n".into() } else { format!("other {i} content for the second file\n") }, 300));
    write("sub/s.txt", lines(&|i| if i == 9 { "sub nine\n".into() } else { format!("sub {i}\n") }, 200));
    ok(&src, 1_700_000_200, &["commit", "-qam", "three"]);
    write("big.txt", big(&[(7, "seven"), (200, "two hundred"), (300, "three hundred")]));
    ok(&src, 1_700_000_300, &["commit", "-qam", "four"]);
    ok(&src, 1_700_000_400, &["checkout", "-q", "-b", "side", "HEAD~2"]);
    write("big.txt", big(&[(7, "seven"), (50, "fifty")]));
    write("sub/s.txt", lines(&|i| if i == 100 { "sub hundred\n".into() } else { format!("sub {i}\n") }, 200));
    ok(&src, 1_700_000_400, &["commit", "-qam", "side1"]);
    ok(&src, 1_700_000_400, &["tag", "-a", "-m", "tagmsg", "vside"]);
    ok(&src, 1_700_000_400, &["checkout", "-q", "main"]);
    ok(&src, 1_700_000_500, &["merge", "-q", "--no-edit", "side"]);
    // The edge commit `two`'s blobs end up stored as deltas on their newer
    // versions, so a preferred base is a reusable delta in the source.
    ok(&src, 0, &["repack", "-adq"]);
    Fixture { root }
}

/// A stock service program, so the receiving side is stock for both clients.
fn service(stock: &str, name: &str) -> String {
    let out = Command::new(stock).arg("--exec-path").output().unwrap();
    format!("{}/git-{name}", String::from_utf8_lossy(&out.stdout).trim())
}

/// Every pack the server gained over `before`, by object id, type, size and delta base, as
/// stock `verify-pack -v` reports them: what was sent plus what `--fix-thin`
/// appended. Sorted, because pack names are content hashes.
fn server_packs(stock: &str, root: &Path, before: &[PathBuf]) -> Vec<String> {
    let dir = root.join("srv/s.git/objects/pack");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "idx") || before.contains(&path) {
            continue;
        }
        let v = git(stock, root, root, &["verify-pack", "-v", path.to_str().unwrap()]);
        let mut objects: Vec<String> = String::from_utf8_lossy(&v.stdout)
            .lines()
            .filter(|l| l.len() > 41 && l.as_bytes()[40] == b' ')
            .map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                format!("{} {} {} {}", f[0], f[1], f[2], f[5..].join(" "))
            })
            .collect();
        objects.sort();
        out.push(format!("{} objects\n{}", objects.len(), objects.join("\n")));
    }
    out.sort();
    out
}

/// `Writing objects: 100% (n/n), <size> | <rate>, done.`: the rate is a clock
/// reading, so it is cut off.
fn steady(stderr: &[u8], root: &Path) -> String {
    String::from_utf8_lossy(stderr)
        .replace(root.to_str().unwrap(), "ROOT")
        .split_inclusive(['\r', '\n'])
        .map(|piece| match piece.find(" | ") {
            Some(at) if piece.starts_with("Writing objects") => format!("{}{}", &piece[..at], &piece[piece.len() - 1..]),
            _ => piece.to_owned(),
        })
        .collect()
}

#[derive(Debug, PartialEq)]
struct Push {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    refs: String,
    packs: Vec<String>,
    fsck: String,
}

/// `args` run in `src` of a fresh fixture by `bin`; `{url}` in an argument is
/// replaced by the destination `dest` names.
fn push(stock: &str, bin: &str, tag: &str, dest: &str, args: &[&str]) -> Push {
    let fx = fixture(stock, tag);
    let root = &fx.root;
    let src = root.join("src");
    let url = match dest {
        "path" => "../srv/s.git".to_owned(),
        "file" => format!("file://{}", root.join("srv/s.git").display()),
        _ => {
            let (port, _) = cgi_server::spawn(stock, root.join("srv"), cgi_server::Fault::NONE);
            format!("http://127.0.0.1:{port}/s.git")
        }
    };
    let receive_pack = format!("--receive-pack={}", service(stock, "receive-pack"));
    let args: Vec<String> = args
        .iter()
        .map(|a| match *a {
            "{url}" => url.clone(),
            "{rp}" => receive_pack.clone(),
            a => a.to_owned(),
        })
        .collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let before = idx_files(root);
    let out = git(bin, &src, root, &args);
    let srv = root.join("srv/s.git");
    Push {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).replace(root.to_str().unwrap(), "ROOT"),
        // The port is the server's own, a fresh one each run.
        stderr: steady(&out.stderr, root).replace(&url, "URL"),
        refs: String::from_utf8_lossy(&git(stock, &srv, root, &["for-each-ref"]).stdout).into_owned(),
        packs: server_packs(stock, root, &before),
        fsck: String::from_utf8_lossy(&git(stock, &srv, root, &["fsck"]).stderr).into_owned(),
    }
}

fn compare_push(stock: &str, tag: &str, dest: &str, args: &[&str]) -> Push {
    let want = push(stock, stock, &format!("{tag}-s"), dest, args);
    let got = push(stock, ZVCS, &format!("{tag}-z"), dest, args);
    assert_eq!(got, want, "{tag}: zvcs and stock git differ");
    want
}

/// Object count of the one pack the server gained.
fn kept(run: &Push) -> &str {
    assert_eq!(run.packs.len(), 1, "{run:?}");
    run.packs[0].split('\n').next().unwrap()
}

fn idx_files(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root.join("srv/s.git/objects/pack")).map_or(Vec::new(), |dir| dir.flatten().map(|e| e.path()).collect())
}

#[test]
fn push_parity_thin_pack_is_the_default_and_no_thin_turns_it_off() {
    let Some(stock) = stock_git() else { return };
    for dest in ["path", "file"] {
        let rp = "{rp}";
        // `four` and `three` lean on `two`'s three blobs, which `--fix-thin`
        // appends: 9 sent, 12 kept.
        let run = compare_push(stock, &format!("thin-{dest}"), dest, &["push", rp, "{url}", "main~1:main"]);
        assert_eq!(kept(&run), "12 objects", "{run:?}");
        let run = compare_push(stock, &format!("nothin-{dest}"), dest, &["push", "--no-thin", rp, "{url}", "main~1:main"]);
        assert_eq!(kept(&run), "9 objects", "{run:?}");
        // The merge, the side branch and its annotated tag in one pack, with
        // every meter and the `Total …` line `--all-progress` draws.
        compare_push(stock, &format!("merge-{dest}"), dest, &["push", "--progress", rp, "{url}", "main", "side", "vside"]);
        compare_push(stock, &format!("nothin-merge-{dest}"), dest, &["push", "--progress", "--no-thin", rp, "{url}", "main", "side"]);
    }
}

#[test]
fn push_parity_thin_pack_over_smart_http_ignores_no_thin() {
    let Some(stock) = stock_git() else { return };
    // `--no-thin` never reaches the helper: `TRANS_OPT_THIN` is among its
    // unsupported options (transport-helper.c:275-280) and remote-curl keeps
    // `options.thin = 1`.
    for (tag, args) in [
        ("http", &["push", "{url}", "main~1:main"][..]),
        ("http-nothin", &["push", "--no-thin", "{url}", "main~1:main"][..]),
    ] {
        let run = compare_push(stock, tag, "http", args);
        assert_eq!(kept(&run), "12 objects", "{run:?}");
    }
    compare_push(stock, "http-merge", "http", &["push", "--progress", "{url}", "main", "side", "vside"]);
}

#[test]
fn push_parity_thin_pack_send_pack_only_with_thin() {
    let Some(stock) = stock_git() else { return };
    let run = compare_push(stock, "sp", "path", &["send-pack", "{rp}", "../srv/s.git", "side"]);
    assert_eq!(kept(&run), "5 objects", "{run:?}");
    let run = compare_push(stock, "sp-thin", "path", &["send-pack", "--thin", "{rp}", "../srv/s.git", "side"]);
    assert_eq!(kept(&run), "7 objects", "{run:?}");
}

#[test]
fn pack_objects_parity_thin_revs_stdout() {
    let Some(stock) = stock_git() else { return };
    let fx = fixture(stock, "po");
    let root = &fx.root;
    let src = root.join("src");
    let inputs: [&[u8]; 7] = [
        b"main~1\n--not\nmain~3\n\n",
        b"main\n--not\nmain~3\n\n",
        b"main\n^side~1\n\n",
        b"main\nvside\n--not\nmain~4\n\n",
        b"side~1..main\n\n",
        b"main\n--not\nmain~2\nside\n\n",
        b"main\n--not\nvside\n\n",
    ];
    for stdin in inputs {
        for extra in [&["--thin"][..], &["--thin", "--delta-base-offset"], &["--thin", "--shallow"], &[]] {
            let mut args = vec!["-c", "pack.threads=1", "pack-objects", "--revs", "--stdout"];
            args.extend_from_slice(extra);
            let want = git_in(stock, &src, root, stdin, &args);
            let got = git_in(ZVCS, &src, root, stdin, &args);
            let what = format!("{:?} {extra:?}", String::from_utf8_lossy(stdin));
            assert_eq!(got.status.code(), want.status.code(), "{what}");
            assert_eq!(String::from_utf8_lossy(&got.stderr), String::from_utf8_lossy(&want.stderr), "{what}");
            assert!(got.stdout == want.stdout, "{what}: the packs differ");
        }
    }
    // `--all-progress`: the preferred bases tick `Enumerating objects` and are
    // among what `Counting objects` counts.
    let args = ["-c", "pack.threads=1", "pack-objects", "--revs", "--stdout", "--thin", "--all-progress"];
    let stdin = b"main\n--not\nmain~3\n\n";
    let want = git_in(stock, &src, root, stdin, &args);
    let got = git_in(ZVCS, &src, root, stdin, &args);
    assert_eq!(steady(&got.stderr, root), steady(&want.stderr, root));
    assert!(got.stdout == want.stdout);
}
